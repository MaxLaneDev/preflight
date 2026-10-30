export function validateCISources(sources){
 const names=new Set();
 return sources.map(source=>{
  const error={};
  if(!/^[A-Za-z0-9_-]{1,64}$/.test(source.name)||names.has(source.name))error.name='Use a unique name: letters, digits, _ or -.';
  names.add(source.name);
  if(!/^[0-9a-f-]{36}$/i.test(source.project_id||''))error.project_id='Choose a registered workspace.';
  if(!['github','gitlab'].includes(source.provider))error.provider='Choose GitHub Actions or GitLab CI.';
  const parts=(source.repository||'').split('/');
  if((source.repository||'').length>256||!parts.length||parts.some(p=>!p||p==='.'||p==='..'||!/^[A-Za-z0-9_.-]+$/.test(p))||(source.provider==='github'&&parts.length!==2))error.repository=source.provider==='github'?'Use owner/repository, for example org/compiler.':'Use namespace/project or a numeric GitLab project ID.';
  if(source.token_env&&!/^[A-Z_][A-Z0-9_]{0,127}$/.test(source.token_env))error.token_env='Enter an environment variable name, never the credential value.';
  if(source.poll_seconds!=null&&(!Number.isSafeInteger(source.poll_seconds)||source.poll_seconds<60||source.poll_seconds>86400))error.poll_seconds='Use whole seconds from 60 to 86400, or leave empty for manual import.';
  if(source.api_base){try{const url=new URL(source.api_base);const local=source.allow_insecure_loopback&&url.protocol==='http:'&&['127.0.0.1','[::1]'].includes(url.hostname);if((url.protocol!=='https:'&&!local)||url.username||url.password||url.search||url.hash)error.api_base='Use an HTTPS API base URL without credentials, query or fragment.';}catch{error.api_base='Enter a valid HTTPS API base URL.';}}
  return error;
 });
}
export function installCISettings({form,values,data,field,el,button,populateProjects,onChange,projectId,addNew=false}){
 if(!data.ci_sources_supported)return {read:()=>values.ci_sources||[],validate:()=>true};
 const section=el('fieldset',undefined,'settings-ci'),rows=[];
 section.append(el('legend','External CI'),el('p','Import completed results only. Provider credentials stay in server environment variables. Empty polling interval means manual import.','settings-help'));
 const cards=el('div'),empty=el('p','No external CI sources.','muted');cards.append(empty);let serial=0;
 const add=button('+ Add CI source',()=>{addSource({name:'',project_id:projectId||'',provider:'github',repository:''});onChange();});
 section.append(cards,add);form.append(section);
 function addSource(source){
  empty.hidden=true;const card=el('section',undefined,'ci-source-card'),fields=el('div',undefined,'ci-source-fields'),controls=new Map(),head=el('div',undefined,'heading');
  const row={source:structuredClone(source),controls,card};
  head.append(el('strong',source.name||'New CI source'),button('Remove source',()=>{card.remove();rows.splice(rows.indexOf(row),1);empty.hidden=rows.length>0;onChange();}));
  card.append(head,fields);cards.append(card);rows.push(row);
  for(const definition of [
   {key:'name',label:'Source name',required:true,format:'1–64 letters, digits, _ or -.',example:'compiler-actions',help:'Stable local name used by MCP and imports.'},
   {key:'project_id',label:'Workspace',type:'select',required:true,format:'Registered local workspace.',example:'Compiler',help:'Owns imported runs, evidence and downloads.'},
   {key:'provider',label:'CI provider',type:'select',required:true,format:'GitHub Actions or GitLab CI.',example:'GitHub Actions',help:'Only provider GET requests are used.'},
   {key:'repository',label:'Repository / project',required:true,format:'GitHub: owner/repository. GitLab: namespace/project or numeric project ID.',example:'org/compiler',help:'Existing provider repository; no workflow is created or triggered.'},
   {key:'token_env',label:'Token environment variable',format:'Environment variable name. Empty = anonymous.',example:'PREFLIGHT_CI_TOKEN',help:'Set its value in the server environment. Never paste a credential into this field.'},
   {key:'poll_seconds',label:'Import interval',type:'number',min:60,max:86400,format:'Whole seconds: 60–86400. Empty = manual.',example:'300',help:'Periodically imports completed provider results; it does not schedule builds.'},
   {key:'test_artifacts',label:'JUnit artifact names',type:'lines',format:'Exact artifact names, one per line. Optional for GitHub.',example:'junit-results',help:'Read bounded XML reports from selected ZIP archives. GitLab published test reports are imported through its API.'},
   {key:'api_base',label:'API base URL',format:'HTTPS URL. Empty = github.com or gitlab.com API.',example:'https://gitlab.example.com/api/v4/',help:'Set only for a self-hosted provider or GitHub Enterprise.'},
   {key:'download_hosts',label:'Extra artifact hosts',type:'lines',format:'Exact DNS hostnames, one per line. Optional.',example:'artifacts.example.com',help:'Allows redirected downloads from an explicit object store. API credentials are never forwarded across origins.'}
  ])controls.set(definition.key,field(fields,definition,source[definition.key],'ci-'+(++serial)));
  const provider=controls.get('provider').input;provider.append(new Option('GitHub Actions','github'),new Option('GitLab CI','gitlab'));provider.value=source.provider;
  const workspace=controls.get('project_id').input;workspace.append(new Option('Choose workspace…',''));if(source.project_id)workspace.append(new Option('Loading selected workspace…',source.project_id));workspace.value=source.project_id||'';populateProjects(workspace,source.project_id);
 }
 function read(){return rows.map(({source,controls})=>{const result={...source};for(const [key,{input,type}]of controls){const value=input.value.trim();if(!value){delete result[key];continue;}result[key]=type==='number'?Number(value):type==='lines'?value.split('\n').map(v=>v.trim()).filter(Boolean):value;}return result;});}
 function validate(){
  const errors=validateCISources(read());rows.forEach((row,index)=>{for(const [key,control]of row.controls){const message=errors[index][key]||'';control.error.textContent=message;control.error.hidden=!message;control.input.setCustomValidity(message);control.input.setAttribute('aria-invalid',String(Boolean(message)));}});
  return errors.every(row=>!Object.keys(row).length);
 }
 for(const source of values.ci_sources||[])addSource(source);if(addNew)addSource({name:'',project_id:projectId||'',provider:'github',repository:''});
 return {read,validate};
}
