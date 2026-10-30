export function validateAccessTokens(tokens){
 const names=new Set(),variables=new Set();
 return tokens.map(token=>{const errors={};
  if(!/^[A-Za-z0-9_-]{1,64}$/.test(token.name)||names.has(token.name))errors.name='Use a unique name: 1–64 letters, digits, _ or -.';
  if(!/^[A-Z_][A-Z0-9_]{0,127}$/.test(token.token_env)||variables.has(token.token_env))errors.token_env='Use a unique uppercase environment variable name, up to 128 characters.';
  if(!['read_only','execute','admin'].includes(token.role))errors.role='Choose a role.';
  names.add(token.name);variables.add(token.token_env);return errors;
 });
}
export function installAccessSettings({form,values,enabled,field,el,button,onChange}){
 if(!enabled)return {read:()=>values.access_tokens||[],validate:()=>true};
 const section=el('fieldset'),rows=[],list=el('div');section.append(el('legend','API credentials'),el('p','Names and roles only. Set secret values in the server environment before saving. Empty list enables trusted loopback access. Roles cover all registered projects; Execute permits trusted native commands.','settings-help'),el('p','Role changes apply immediately. Keep an Admin credential available. Removing this client’s credential disconnects it; active pipelines continue.','settings-format'),list);
 const add=button('+ Add API credential',()=>addRow({name:'',token_env:'',role:'read_only'}));section.append(add);form.append(section);let serial=0;
 function addRow(token){
  const card=el('section',undefined,'settings-policy'),controls=new Map(),row={card,controls};rows.push(row);
  const remove=button('Remove credential',()=>{rows.splice(rows.indexOf(row),1);card.remove();add.disabled=rows.length>=32;onChange();});card.append(remove);
  for(const definition of [{key:'name',label:'Credential name',required:true,example:'codex',format:'Unique ASCII name. No secret value.'},{key:'token_env',label:'Secret environment variable',required:true,example:'PREFLIGHT_CODEX_TOKEN',format:'Uppercase environment name, not the token. Its server-side value must contain 32–512 bearer-token characters.'},{key:'role',label:'Access role',type:'select',required:true,format:'Read only: evidence. Execute: builds and cancellation. Admin: configuration, cleanup, worker enrollment and hook bindings.'}])controls.set(definition.key,field(card,definition,token[definition.key],'access-'+(++serial)));
  const role=controls.get('role').input;role.append(new Option('Read only','read_only'),new Option('Execute','execute'),new Option('Admin','admin'));role.value=token.role;list.append(card);add.disabled=rows.length>=32;onChange();
 }
 function read(){return rows.map(({controls})=>Object.fromEntries([...controls].map(([key,{input}])=>[key,input.value.trim()])));}
 function validate(){const errors=validateAccessTokens(read());rows.forEach((row,index)=>{for(const [key,control]of row.controls){const message=errors[index][key]||'';control.error.textContent=message;control.error.hidden=!message;control.input.setCustomValidity(message);control.input.setAttribute('aria-invalid',String(Boolean(message)));}});return rows.length<=32&&errors.every(e=>!Object.keys(e).length);}
 // Initial rows are hydrated without firing form validation before it exists.
 const savedChange=onChange;onChange=()=>{};for(const token of values.access_tokens||[])addRow(token);onChange=savedChange;
 return {read,validate};
}
