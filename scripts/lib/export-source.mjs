import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import {execFileSync} from 'node:child_process';
const [root,destination]=process.argv.slice(2);
if(fs.existsSync(destination))throw Error('Destination exists');
const roots=new Set(['.gitignore','Cargo.toml','Cargo.lock','README.md','LICENSE','SECURITY.md','CONTRIBUTING.md','PRIVACY.md','preflight.example.toml','install.sh']);
const directories=new Set(['.github','assets','crates','docs','examples','integrations','skills','packaging','migrations','Formula']);
const scripts=new Set(['release.sh','release-files.sh','install-preflight.sh','smoke-package.sh','test-release.sh','test-script-entrypoints.sh','client-notices.mjs','package-codex-plugin.sh','test-codex-plugin.sh','test-install-sh.sh','test-client.sh','test-client.cjs','test-launchd.sh','accept-homebrew.sh','export-public-source.sh','test-public-export.sh','homebrew-release.sh']);
const libraries=new Set(['common.sh','acceptance.sh','fixture.mjs','public-acceptance.mjs','run-public.mjs','run-bounded.mjs','export-source.mjs','extract-release-binary.sh']);
const paths=[...new Set(execFileSync('git',['ls-files','--cached','--others','--exclude-standard','-z'],{cwd:root}).toString().split('\0'))].sort();
const selected=[];
for(const relative of paths){
 if(!relative)continue;const parts=relative.split('/'),file=path.join(root,relative);
 if(!(roots.has(relative)||directories.has(parts[0])||parts[0]==='scripts'&&(scripts.has(relative.slice(8))||relative.startsWith('scripts/lib/')&&libraries.has(path.basename(relative)))))continue;
 if(parts[0]==='examples'&&!['go-basic','c-basic','cpp-basic'].includes(parts[1]))continue;
 if(!fs.existsSync(file))continue;
 if(fs.lstatSync(file).isSymbolicLink())throw Error('Publication refuses symlink: '+relative);
 if(!fs.statSync(file).isFile())continue;
 const data=fs.readFileSync(file);
 if(data.includes(Buffer.from(os.homedir()+'/'))||/gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|-----BEGIN (?:OPENSSH |RSA |EC )?PRIVATE KEY-----/.test(data.toString()))throw Error('Review private data before export: '+relative);
 selected.push(relative);
}
for(const required of ['Cargo.toml','.github/workflows/ci.yml','.github/workflows/release.yml','LICENSE','README.md','install.sh'])if(!selected.includes(required))throw Error('Missing source: '+required);
fs.mkdirSync(destination,{recursive:true});
for(const relative of selected){const target=path.join(destination,relative);fs.mkdirSync(path.dirname(target),{recursive:true});fs.copyFileSync(path.join(root,relative),target,fs.constants.COPYFILE_EXCL);fs.chmodSync(target,fs.statSync(path.join(root,relative)).mode&0o777);}
const plugin=path.join(destination,'integrations/codex');
fs.cpSync(path.join(root,'skills/preflight-mcp'),path.join(plugin,'skills/preflight-mcp'),{recursive:true});
fs.mkdirSync(path.join(plugin,'assets'),{recursive:true});fs.copyFileSync(path.join(root,'assets/branding/preflight-icon.png'),path.join(plugin,'assets/icon.png'));
fs.mkdirSync(path.join(destination,'.agents/plugins'),{recursive:true});
fs.writeFileSync(path.join(destination,'.agents/plugins/marketplace.json'),JSON.stringify({name:'preflight',interface:{displayName:'Preflight'},plugins:[{name:'preflight',source:{source:'local',path:'./integrations/codex'},policy:{installation:'AVAILABLE',authentication:'ON_INSTALL'},category:'Productivity'}]},null,2)+'\n');
for(const relative of selected.filter(n=>n.startsWith('crates/')&&n.endsWith('.rs'))){const rust=path.join(destination,relative);for(const match of fs.readFileSync(rust,'utf8').matchAll(/include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)/g))if(!fs.existsSync(path.resolve(path.dirname(rust),match[1])))throw Error('Missing compile-time input: '+relative+' -> '+match[1]);}
console.log(JSON.stringify({files:selected.length,destination:path.resolve(destination),history_included:false}));
