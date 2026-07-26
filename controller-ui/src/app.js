// Frontend de la app controladora. Habla con el backend Rust (Tauri)
// via invoke() para mandar comandos, y escucha eventos para lo que
// llega de la red (video, emparejamiento, errores, archivos).

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const connectScreen = document.getElementById("connect-screen");
const sessionScreen = document.getElementById("session-screen");
const signalingUrlInput = document.getElementById("signaling-url");
const agentCodeInput = document.getElementById("agent-code");
const connectBtn = document.getElementById("connect-btn");
const connectStatus = document.getElementById("connect-status");
const sessionStatus = document.getElementById("session-status");
const video = document.getElementById("video");
const filePathInput = document.getElementById("file-path-input");
const sendFileBtn = document.getElementById("send-file-btn");
const restartBtn = document.getElementById("restart-btn");
const disconnectBtn = document.getElementById("disconnect-btn");

// --- Mapeo de tecla fisica (event.code) a Virtual-Key Code de Windows.
// Mismo mapeo que usa dev_viewer del lado Rust, para que ambos manden
// los mismos codigos al agent.
const CODE_TO_VK = {
  KeyA: 0x41, KeyB: 0x42, KeyC: 0x43, KeyD: 0x44, KeyE: 0x45, KeyF: 0x46,
  KeyG: 0x47, KeyH: 0x48, KeyI: 0x49, KeyJ: 0x4A, KeyK: 0x4B, KeyL: 0x4C,
  KeyM: 0x4D, KeyN: 0x4E, KeyO: 0x4F, KeyP: 0x50, KeyQ: 0x51, KeyR: 0x52,
  KeyS: 0x53, KeyT: 0x54, KeyU: 0x55, KeyV: 0x56, KeyW: 0x57, KeyX: 0x58,
  KeyY: 0x59, KeyZ: 0x5A,
  Digit0: 0x30, Digit1: 0x31, Digit2: 0x32, Digit3: 0x33, Digit4: 0x34,
  Digit5: 0x35, Digit6: 0x36, Digit7: 0x37, Digit8: 0x38, Digit9: 0x39,
  Space: 0x20, Enter: 0x0D, Backspace: 0x08, Tab: 0x09, Escape: 0x1B,
  ArrowLeft: 0x25, ArrowUp: 0x26, ArrowRight: 0x27, ArrowDown: 0x28,
  Delete: 0x2E, Home: 0x24, End: 0x23, PageUp: 0x21, PageDown: 0x22,
  ShiftLeft: 0xA0, ShiftRight: 0xA1, ControlLeft: 0xA2, ControlRight: 0xA3,
  AltLeft: 0xA4, AltRight: 0xA5, CapsLock: 0x14,
  F1: 0x70, F2: 0x71, F3: 0x72, F4: 0x73, F5: 0x74, F6: 0x75,
  F7: 0x76, F8: 0x77, F9: 0x78, F10: 0x79, F11: 0x7A, F12: 0x7B,
};

function showStatus(el, message, kind) {
  el.textContent = message;
  el.className = "status " + (kind || "");
}

function showSession() {
  connectScreen.classList.add("hidden");
  sessionScreen.classList.remove("hidden");
}

function showConnectScreen() {
  sessionScreen.classList.add("hidden");
  connectScreen.classList.remove("hidden");
  connectBtn.disabled = false;
}

// --- Conectar ---
connectBtn.addEventListener("click", async () => {
  const url = signalingUrlInput.value.trim();
  const code = agentCodeInput.value.trim();
  if (!url || !code) {
    showStatus(connectStatus, "Completá el servidor y el código.", "error");
    return;
  }

  connectBtn.disabled = true;
  showStatus(connectStatus, "Conectando...", "info");

  try {
    await invoke("connect", { url, code });
    sessionStatus.textContent = `Esperando video de ${code}...`;
    showSession();
  } catch (e) {
    showStatus(connectStatus, "Error: " + e, "error");
    connectBtn.disabled = false;
  }
});

// --- Eventos que llegan de la red ---
listen("paired", () => {
  sessionStatus.textContent = "Emparejado - esperando el primer frame...";
});

listen("peer-disconnected", () => {
  sessionStatus.textContent = "Se desconectó.";
  showConnectScreen();
  showStatus(connectStatus, "La conexión se cortó.", "error");
});

listen("connection-error", (event) => {
  showStatus(connectStatus, "Error: " + event.payload.message, "error");
});

listen("video-frame", (event) => {
  video.src = "data:image/jpeg;base64," + event.payload;
  sessionStatus.textContent = "Conectado";
});

listen("restart-ack", () => {
  sessionStatus.textContent = "La PC remota se está reiniciando...";
});

listen("file-incoming", (event) => {
  sessionStatus.textContent = `Recibiendo ${event.payload.name}...`;
});

listen("file-received", (event) => {
  sessionStatus.textContent = `Archivo recibido: ${event.payload}`;
});

// --- Input: mouse ---
video.addEventListener("mousemove", (e) => {
  const rect = video.getBoundingClientRect();
  const x = (e.clientX - rect.left) / rect.width;
  const y = (e.clientY - rect.top) / rect.height;
  if (x < 0 || x > 1 || y < 0 || y > 1) return;
  invoke("send_mouse_move", { x, y }).catch(() => {});
});

const BUTTON_MAP = { 0: 0, 2: 1, 1: 2 }; // JS: 0=left,1=middle,2=right -> nuestro: 0=left,1=right,2=middle

video.addEventListener("mousedown", (e) => {
  e.preventDefault();
  invoke("send_mouse_button", { button: BUTTON_MAP[e.button] ?? 0, pressed: true }).catch(() => {});
});

video.addEventListener("mouseup", (e) => {
  e.preventDefault();
  invoke("send_mouse_button", { button: BUTTON_MAP[e.button] ?? 0, pressed: false }).catch(() => {});
});

video.addEventListener("contextmenu", (e) => e.preventDefault());

video.addEventListener("wheel", (e) => {
  e.preventDefault();
  const delta = e.deltaY < 0 ? 120 : -120;
  invoke("send_mouse_wheel", { delta }).catch(() => {});
});

// --- Input: teclado ---
// El <img> necesita tabindex para poder recibir foco y eventos de teclado.
video.addEventListener("keydown", (e) => {
  e.preventDefault();
  const vk = CODE_TO_VK[e.code];
  if (vk !== undefined) {
    invoke("send_key", { vk, pressed: true }).catch(() => {});
  }
});

video.addEventListener("keyup", (e) => {
  e.preventDefault();
  const vk = CODE_TO_VK[e.code];
  if (vk !== undefined) {
    invoke("send_key", { vk, pressed: false }).catch(() => {});
  }
});

video.addEventListener("click", () => video.focus());

// --- Reinicio remoto ---
restartBtn.addEventListener("click", () => {
  if (!confirm("¿Reiniciar la PC remota ahora?")) return;
  invoke("send_restart", { delaySecs: 5 }).catch((e) => alert("Error: " + e));
});

// --- Transferencia de archivos ---
sendFileBtn.addEventListener("click", async () => {
  const path = filePathInput.value.trim();
  if (!path) return;
  sendFileBtn.disabled = true;
  sessionStatus.textContent = `Enviando ${path}...`;
  try {
    await invoke("send_file", { path });
    sessionStatus.textContent = "Archivo enviado.";
    filePathInput.value = "";
  } catch (e) {
    alert("Error enviando el archivo: " + e);
  } finally {
    sendFileBtn.disabled = false;
  }
});

// --- Desconectar ---
disconnectBtn.addEventListener("click", () => {
  // Por ahora, "desconectar" del lado del controller simplemente
  // recarga la app - el backend detecta el cierre de la conexion
  // WebSocket y limpia su estado solo. Mas adelante se puede agregar
  // un comando `disconnect` explicito si hace falta mas prolijidad.
  window.location.reload();
});
