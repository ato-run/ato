'use strict';
// Minimal frontend build: creates dist/ using only Node.js built-ins.
// Verifies that Node (from runtime_tools) is available on PATH.
const fs = require('fs');

fs.mkdirSync('dist/assets', { recursive: true });
fs.writeFileSync('dist/index.html', fs.readFileSync('index.html', 'utf8'));
fs.writeFileSync('dist/assets/bundle.js', '"use strict";\nconsole.log("fixture-bundle");\n');

process.stdout.write('dist-ok\n');
