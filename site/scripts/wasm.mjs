// Builds Fennec's interpreter as WebAssembly for the playground and copies it into public/.
// `npm run dev` and `npm run build` run this first, so the site never needs a separate step.
import { spawnSync } from 'node:child_process';
import { copyFileSync } from 'node:fs';

const target = 'wasm32-unknown-unknown';
const built = new URL(`../../target/${target}/release/fennec_wasm.wasm`, import.meta.url);
const destination = new URL('../public/fennec.wasm', import.meta.url);

const cargo = spawnSync('cargo', ['build', '--release', '--locked', '--target', target, '-p', 'fennec-wasm'], {
  stdio: 'inherit',
});
if (cargo.error || cargo.status !== 0) {
  console.error(
    cargo.error
      ? `couldn't run cargo (${cargo.error.message}): the playground needs Rust, from https://rustup.rs`
      : `building the playground failed; if the ${target} target is missing, run: rustup target add ${target}`,
  );
  process.exit(1);
}
copyFileSync(built, destination);
