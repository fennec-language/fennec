// Runs Fennec programs off the page's thread, so a long run never freezes the tab. The page
// sends { action: 'run' | 'test', source, wasm } and gets back { ok, parts }, as described in
// crates/wasm/src/lib.rs.

let compiled;

self.onmessage = async ({ data }) => {
  try {
    compiled ??= await compile(data.wasm);
    self.postMessage(call(data.action, data.source));
  } catch (error) {
    self.postMessage({ ok: false, parts: [['err', describe(error)]] });
  }
};

async function compile(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url} answered ${response.status}`);
  return WebAssembly.compile(await response.arrayBuffer());
}

// Each run gets a fresh instance, so nothing one program does can leak into the next.
function call(action, source) {
  const { exports } = new WebAssembly.Instance(compiled, {});
  const bytes = new TextEncoder().encode(source);
  const at = exports.fennec_input(bytes.length);
  new Uint8Array(exports.memory.buffer, at, bytes.length).set(bytes);
  const len = exports[`fennec_${action}`]();
  const result = new Uint8Array(exports.memory.buffer, exports.fennec_output(), len);
  return JSON.parse(new TextDecoder().decode(result));
}

function describe(error) {
  if (error instanceof RangeError) {
    return "error: the program ran out of stack\n  = help: the browser's stack is small; check for recursion that goes deep\n";
  }
  if (error instanceof WebAssembly.RuntimeError) {
    return `error: the interpreter stopped unexpectedly (${error.message})\n  = help: this is a bug in Fennec; please report it at https://github.com/fennec-language/fennec/issues\n`;
  }
  return `error: couldn't start the interpreter: ${error.message}\n`;
}
