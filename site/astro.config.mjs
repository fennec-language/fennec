import { readFileSync } from 'node:fs';
import { defineConfig } from 'astro/config';

// Fennec's own TextMate grammar, so code blocks highlight as Fennec rather than borrowing another language's.
const fennec = JSON.parse(readFileSync(new URL('./src/fennec.tmLanguage.json', import.meta.url), 'utf8'));

export default defineConfig({
  site: 'https://seashell86.github.io',
  base: '/fennec/',
  trailingSlash: 'always',
  markdown: {
    shikiConfig: {
      themes: { light: 'vitesse-light', dark: 'vitesse-dark' },
      langs: [fennec],
    },
  },
});
