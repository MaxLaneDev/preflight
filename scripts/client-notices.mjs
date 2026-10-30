// Generate redistributable license copies for the pinned editor bundle, without network access.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import {fileURLToPath} from 'node:url';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..','crates','preflight-client');
const digest=file=>crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const lock=JSON.parse(fs.readFileSync(path.join(root,'package-lock.json'),'utf8')),packages=[],files={};
for(const [relative,entry]of Object.entries(lock.packages)){
 if(!relative.startsWith('node_modules/')||entry.dev)continue;
 const directory=path.join(root,relative),pkg=JSON.parse(fs.readFileSync(path.join(directory,'package.json'),'utf8'));
 if(pkg.version!==entry.version)throw new Error('Installed dependency differs from lock: '+pkg.name);
 const copies=[];
 for(const name of fs.readdirSync(directory).sort()){
  const file=path.join(directory,name);if(!/^(license|licence|copying|notice)/i.test(name)||!fs.lstatSync(file).isFile())continue;
  const key='licenses/npm/'+pkg.name.replace('/','_')+'-'+pkg.version+'/'+name;files[key]=fs.readFileSync(file,'utf8');copies.push(key);
 }
 if(!copies.length)throw new Error('Missing license text for '+pkg.name);
 packages.push({ecosystem:'npm',name:pkg.name,version:pkg.version,license:pkg.license,files:copies});
}
const result={format:'preflight-js-notices-v1',lock_sha256:digest(path.join(root,'package-lock.json')),bundle_sha256:digest(path.join(root,'src/editor-bundle.mjs')),packages,files};
fs.writeFileSync(path.join(root,'THIRD_PARTY_NOTICES.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify({packages:packages.length,license_files:Object.keys(files).length}));
