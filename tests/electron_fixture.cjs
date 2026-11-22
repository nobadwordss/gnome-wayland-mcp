const { app, BrowserWindow, Menu } = require('electron');
const { renameSync, writeFileSync } = require('node:fs');

const report = process.argv.at(-1);
const page = `<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy"
      content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'">
<title>Private MCP Electron smoke test</title>
<style>
  body { margin: 0; background: #23354b; }
  button, textarea { position: absolute; border: 0; color: #fff; font: 18px sans-serif; }
  button { left: 24px; top: 16px; width: 168px; height: 52px; background: #d84292; }
  textarea { left: 190px; top: 124px; width: 300px; height: 128px;
             background: #187b73; padding: 12px; resize: none; }
</style>
<button type="button">Click target</button>
<textarea aria-label="Paste target" spellcheck="false"></textarea>
<script>
  const button = document.querySelector('button');
  const editor = document.querySelector('textarea');
  let clicks = 0;
  let pastes = 0;
  button.addEventListener('click', event => { if (event.isTrusted) clicks++; });
  editor.addEventListener('paste', event => { if (event.isTrusted) pastes++; });
  window.readSmokeState = () => ({
    clicks,
    pastes,
    focused: document.hasFocus() && document.activeElement === editor,
    text: editor.value,
    selected: editor.value.slice(editor.selectionStart, editor.selectionEnd),
  });
</script>
</html>`;

async function run() {
    Menu.setApplicationMenu(null);
    const window = new BrowserWindow({ width: 700, height: 400, show: false });
    await window.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(page)}`);
    window.show();

    const environment = {
        pid: process.pid,
        electron: process.versions.electron,
        runtime: process.env.XDG_RUNTIME_DIR,
        wayland: process.env.WAYLAND_DISPLAY,
        waylandSocket: process.env.WAYLAND_SOCKET ?? null,
        display: process.env.DISPLAY ?? null,
        sessionType: process.env.XDG_SESSION_TYPE,
        ozone: app.commandLine.getSwitchValue('ozone-platform'),
        profile: app.getPath('userData'),
    };
    let lastReport;
    while (!window.isDestroyed()) {
        const renderer = window.webContents.getOSProcessId();
        const state = await window.webContents.executeJavaScript('window.readSmokeState()');
        const processes = app.getAppMetrics().map(({ pid }) => pid);
        const next = JSON.stringify({ ...environment, renderer, processes, ...state });
        if (next !== lastReport) {
            writeFileSync(`${report}.tmp`, next);
            renameSync(`${report}.tmp`, report);
            lastReport = next;
        }
        await new Promise(resolve => setTimeout(resolve, 50));
    }
}

app.whenReady().then(run).catch(error => {
    console.error(error);
    app.exit(1);
});
