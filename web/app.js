import init, { convert_text } from "./pkg/cfgprism.js";

const FORMATS = [
  "json", "jsonc", "json5", "toml", "yaml",
  "dotenv", "ini", "hcl", "properties", "kdl", "ron",
];

const DEFAULT_INPUT = `# cfgprism web demo
server: &srv
  host: example.com # shared anchor
  port: 8080
clients:
  - one
  - *srv
`;

const fromSel = document.getElementById("from");
const toSel = document.getElementById("to");
const inputEl = document.getElementById("input");
const outputEl = document.getElementById("output");
const warningsEl = document.getElementById("warnings");
const warnCountEl = document.getElementById("warn-count");
const errorEl = document.getElementById("error");
const statusEl = document.getElementById("status");

for (const f of FORMATS) {
  fromSel.add(new Option(f, f));
  toSel.add(new Option(f, f));
}

function b64encode(s) {
  return btoa(String.fromCharCode(...new TextEncoder().encode(s)))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");
}

function b64decode(s) {
  s = s.replaceAll("-", "+").replaceAll("_", "/");
  while (s.length % 4) s += "=";
  const bytes = Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

function readHash() {
  const h = new URLSearchParams(location.hash.slice(1));
  return {
    from: h.get("from"),
    to: h.get("to"),
    src: h.get("src") ? b64decode(h.get("src")) : null,
  };
}

function writeHash() {
  const h = new URLSearchParams();
  h.set("from", fromSel.value);
  h.set("to", toSel.value);
  h.set("src", b64encode(inputEl.value));
  history.replaceState(null, "", "#" + h.toString());
}

function convert() {
  errorEl.textContent = "";
  warningsEl.innerHTML = "";
  warnCountEl.textContent = "";
  try {
    const raw = convert_text(fromSel.value, toSel.value, inputEl.value);
    const { text, warnings } = JSON.parse(raw);
    outputEl.textContent = text;
    warnCountEl.textContent = warnings.length ? `(${warnings.length})` : "(none)";
    for (const w of warnings) {
      const li = document.createElement("li");
      li.textContent = w;
      warningsEl.appendChild(li);
    }
  } catch (e) {
    outputEl.textContent = "";
    errorEl.textContent = String(e);
  }
  writeHash();
}

let timer = null;
function convertSoon() {
  clearTimeout(timer);
  timer = setTimeout(convert, 150);
}

document.getElementById("swap").addEventListener("click", () => {
  [fromSel.value, toSel.value] = [toSel.value, fromSel.value];
  convert();
});
document.getElementById("copy-link").addEventListener("click", async () => {
  writeHash();
  await navigator.clipboard.writeText(location.href);
  statusEl.textContent = "link copied";
  setTimeout(() => (statusEl.textContent = ""), 2000);
});
fromSel.addEventListener("change", convert);
toSel.addEventListener("change", convert);
inputEl.addEventListener("input", convertSoon);

statusEl.textContent = "loading wasm…";
try {
  await init();
  const state = readHash();
  fromSel.value = FORMATS.includes(state.from) ? state.from : "yaml";
  toSel.value = FORMATS.includes(state.to) ? state.to : "json";
  inputEl.value = state.src ?? DEFAULT_INPUT;
  statusEl.textContent = "";
  convert();
} catch (e) {
  statusEl.textContent = "";
  errorEl.textContent = "failed to load WASM: " + e;
}
