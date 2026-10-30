// Runs every program in examples/ through the playground's WebAssembly module, the way the
// page's worker does, and checks it prints its .out file and passes its tests. Programs that
// use files are skipped: the playground has none.
import { readFileSync, readdirSync } from 'node:fs';

const examples = new URL('../../examples/', import.meta.url);
const compiled = new WebAssembly.Module(readFileSync(new URL('../public/fennec.wasm', import.meta.url)));

function call(action, source) {
  const { exports } = new WebAssembly.Instance(compiled, {});
  const bytes = new TextEncoder().encode(source);
  const at = exports.fennec_input(bytes.length);
  new Uint8Array(exports.memory.buffer, at, bytes.length).set(bytes);
  const len = exports[`fennec_${action}`]();
  return JSON.parse(new TextDecoder().decode(new Uint8Array(exports.memory.buffer, exports.fennec_output(), len)));
}

let failed = 0;
for (const name of readdirSync(examples).filter((file) => file.endsWith('.fen')).sort()) {
  const source = readFileSync(new URL(name, examples), 'utf8');
  if (/\bfs: Fs\b/.test(source)) {
    console.log(`  - ${name} (uses files)`);
    continue;
  }
  const expected = readFileSync(new URL(name.replace(/\.fen$/, '.out'), examples), 'utf8');
  const run = call('run', source);
  const tests = call('test', source);
  const printed = run.parts.map(([, text]) => text).join('');
  if (run.ok && printed === expected && tests.ok) {
    console.log(`  ✓ ${name}`);
  } else {
    failed += 1;
    console.log(`  ✗ ${name}\n${printed}${tests.parts.map(([, text]) => text).join('')}`);
  }
}
process.exit(failed === 0 ? 0 : 1);
