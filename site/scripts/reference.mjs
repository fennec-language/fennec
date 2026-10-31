// Writes the language reference from the examples. It isn't kept in the repository, so adding
// an example can't conflict with anything.
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../..', import.meta.url));
execFileSync('cargo', ['run', '--locked', '--quiet', '-p', 'fennec-reference'], { cwd: root, stdio: 'inherit' });
