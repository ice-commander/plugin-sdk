const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const { version } = JSON.parse(fs.readFileSync(path.join(root, 'package.json'), 'utf8'));

const content = `macro_rules! sdk_version {
    () => {
        "${version}"
    };
}
`;

const out = path.join(root, 'version.rs');
fs.writeFileSync(out, content, 'utf8');
console.log(`Generated version.rs -> v${version}`);
