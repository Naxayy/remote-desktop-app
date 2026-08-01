const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const statusEl = document.getElementById("status");
const urlInput = document.getElementById("signaling-url");
const codeInput = document.getElementById("agent-code");
const saveBtn = document.getElementById("save-btn");
const disconnectBtn = document.getElementById("disconnect-btn");

async function loadConfig() {
  try {
    const cfg = await invoke("get_config");
    urlInput.value = cfg.signaling_url;
    codeInput.value = cfg.agent_code;
  } catch (e) {
    statusEl.textContent = "Error cargando configuración: " + e;
  }
}

listen("status", (event) => {
  statusEl.textContent = event.payload;
});

saveBtn.addEventListener("click", async () => {
  const signaling_url = urlInput.value.trim();
  const agent_code = codeInput.value.trim();
  if (!signaling_url || !agent_code) {
    statusEl.textContent = "Completá el servidor y el código.";
    return;
  }
  saveBtn.disabled = true;
  try {
    await invoke("save_and_reconnect", { signalingUrl: signaling_url, agentCode: agent_code });
  } catch (e) {
    statusEl.textContent = "Error: " + e;
  } finally {
    saveBtn.disabled = false;
  }
});

disconnectBtn.addEventListener("click", async () => {
  try {
    await invoke("disconnect");
  } catch (e) {
    statusEl.textContent = "Error: " + e;
  }
});

loadConfig();
