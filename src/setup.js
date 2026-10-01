const DEMO = "https://onecamp.onemana.dev";
  const form = document.getElementById("f"), input = document.getElementById("url"),
        btn = document.getElementById("go"), err = document.getElementById("err");
  async function connect(url) {
    err.textContent = ""; btn.disabled = true; btn.textContent = "Connecting…";
    try { await window.__TAURI__.core.invoke("open_workspace", { url }); }
    catch (e) { err.textContent = String(e); btn.disabled = false; btn.textContent = "Connect"; }
  }
  form.addEventListener("submit", (e) => { e.preventDefault(); connect(input.value); });
  const demo = document.getElementById("demo");
  demo.addEventListener("click", () => connect(DEMO));
  demo.addEventListener("keydown", (e) => { if (e.key === "Enter") connect(DEMO); });
