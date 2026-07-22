//! Implementacion real de ScreenCapturer para Windows via DXGI Desktop
//! Duplication API. Solo se compila con `cfg(windows)`.
//!
//! Nota: este archivo se escribio sin poder compilarlo en el momento
//! (se desarrollo en un entorno Linux). Probalo en tu PC Windows; si
//! tira algun error de compilacion (nombres de tipos/metodos que
//! cambiaron entre versiones del crate `windows`), pasame el mensaje
//! exacto y lo ajustamos.

use super::Frame;
use anyhow::{Context, Result};
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    IDXGIDevice, IDXGIOutput1, IDXGIOutputDuplication, IDXGIResource, DXGI_OUTDUPL_FRAME_INFO,
};

pub struct ScreenCapturer {
    // Reservado para uso futuro (ej: encoding acelerado por GPU que
    // comparta el mismo device). Por ahora next_frame() no lo necesita
    // directamente porque la textura staging ya se crea una sola vez.
    #[allow(dead_code)]
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    duplication: IDXGIOutputDuplication,
    /// Textura de staging reusada en cada frame - crearla es una
    /// llamada cara al driver de GPU, no tiene sentido repetirla 30
    /// veces por segundo.
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl ScreenCapturer {
    /// Inicializa la captura sobre el monitor primario (indice 0).
    /// Proximo paso, cuando soportemos multi-monitor: parametrizar el
    /// indice de output.
    pub fn new() -> Result<Self> {
        unsafe {
            let mut device: Option<ID3D11Device> = None;
            let mut context: Option<ID3D11DeviceContext> = None;

            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .context("D3D11CreateDevice fallo - GPU/driver no soporta Direct3D11?")?;

            let device = device.context("device nulo despues de D3D11CreateDevice")?;
            let context = context.context("context nulo despues de D3D11CreateDevice")?;

            // device -> DXGI device -> adapter -> output (monitor 0) -> output1 -> duplication
            let dxgi_device: IDXGIDevice = device.cast()?;
            let adapter = dxgi_device.GetAdapter()?;
            let output = adapter.EnumOutputs(0)?;
            let output1: IDXGIOutput1 = output.cast()?;
            let duplication = output1.DuplicateOutput(&device)?;

            let desc = duplication.GetDesc();
            let width = desc.ModeDesc.Width;
            let height = desc.ModeDesc.Height;

            // Textura "staging": copia en la que la CPU si puede leer
            // (la textura original de AcquireNextFrame vive solo en GPU).
            // La creamos una unica vez y la reusamos en cada next_frame().
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            device.CreateTexture2D(&staging_desc, None, Some(&mut staging))?;
            let staging = staging.context("no se pudo crear la textura staging")?;

            Ok(Self {
                device,
                context,
                duplication,
                staging,
                width,
                height,
            })
        }
    }

    /// Bloquea hasta que Windows entregue el proximo frame (o timeout),
    /// lo copia a memoria de CPU y lo devuelve como BGRA8 crudo.
    pub fn next_frame(&mut self) -> Result<Frame> {
        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;

            // timeout de 500ms: si no hay cambios en pantalla, DXGI no
            // entrega frame nuevo dentro de ese tiempo.
            self.duplication
                .AcquireNextFrame(500, &mut frame_info, &mut resource)
                .context("AcquireNextFrame fallo")?;

            let resource = resource.context("resource nulo en AcquireNextFrame")?;
            let texture: ID3D11Texture2D = resource.cast()?;

            self.context.CopyResource(&self.staging, &texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .context("Map de la textura staging fallo")?;

            // El row pitch de la GPU puede tener padding extra, hay que
            // copiar fila por fila respetando el ancho real en bytes.
            let row_bytes = (self.width * 4) as usize;
            let mut data = vec![0u8; row_bytes * self.height as usize];
            let src = mapped.pData as *const u8;
            let src_pitch = mapped.RowPitch as usize;

            for y in 0..self.height as usize {
                let src_row = src.add(y * src_pitch);
                let dst_row = data.as_mut_ptr().add(y * row_bytes);
                std::ptr::copy_nonoverlapping(src_row, dst_row, row_bytes);
            }

            self.context.Unmap(&self.staging, 0);
            self.duplication.ReleaseFrame().ok();

            Ok(Frame {
                width: self.width,
                height: self.height,
                data,
            })
        }
    }
}

// SAFETY: los tipos COM de D3D11/DXGI no son Send/Sync por defecto porque
// el compilador no sabe que los usamos de forma correcta (un solo hilo
// a la vez, sin compartir el capturer entre threads). Por ahora el
// capturer se usa solo desde el hilo que lo crea, asi que no hace falta
// marcarlo Send/Sync manualmente todavia.
