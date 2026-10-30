// Release notes are untrusted text; never render Markdown as HTML or run update commands.
export function releaseSummary(status){
 if(status.status==='disabled')return 'Release notifications are disabled.';
 if(status.status==='unchecked')return 'Waiting for the first release check.';
 if(status.status==='error')return status.error||'Release check failed.';
 return status.update_available?'Preflight '+status.latest_version+' is available.':'Preflight '+status.current_version+' is up to date.';
}
export function releaseLink(value){
 try{const url=new URL(value);return url.protocol==='https:'&&url.hostname==='github.com'&&!url.username&&!url.password&&!url.port&&/^\/[^/]+\/[^/]+\/releases\/tag\/[^/]+$/.test(url.pathname)&&!url.search&&!url.hash?url.href:null;}catch{return null;}
}
export function installReleaseNotice({anchor,api,el,button,modalClose,closeDialog,notice}){
 const toggle=button('',()=>open());toggle.id='release-notice';toggle.className='icon-button release-notice';toggle.hidden=true;
 toggle.innerHTML='<svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true"><path d="M12 16V3m-5 8 5 5 5-5M5 17v4h14v-4"/></svg>';
 anchor.before(toggle);let current=null,pending=null;
 async function refresh(){
  if(document.hidden)return;
  if(pending)return pending;
  pending=(async()=>{try{const status=await api('server-release');current=status;toggle.hidden=!status.update_available;const label=releaseSummary(status);toggle.title=label;toggle.setAttribute('aria-label',label);}catch{/* An unavailable status must not interrupt navigation. */}finally{pending=null;}})();
  return pending;
 }
 async function open(){
  try{const status=await api('server-release?details=true');current=status;
   const dialog=el('dialog',undefined,'release-dialog');dialog.setAttribute('aria-label','Server release');
   const heading=el('div',undefined,'heading');heading.append(el('h2',releaseSummary(status)),modalClose(button('Close',()=>closeDialog(dialog))));
   dialog.append(heading,el('p','Installed: '+status.current_version,'muted'));
   const link=releaseLink(status.release_url);if(link){const source=el('a','View release on GitHub');source.href=link;source.target='_blank';source.rel='noopener noreferrer';dialog.append(source);}
   const notes=el('pre',status.changelog||'No release notes supplied.','release-notes');dialog.append(notes);
   if(status.changelog_truncated)dialog.append(el('p','Release notes shortened. Open GitHub for the complete changelog.','muted'));
   dialog.append(el('p','Notification only. Installation is an operator action; running work continues.','muted'));
   dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();
  }catch(error){notice(error);}
 }
 const visible=()=>{if(!document.hidden)refresh();};document.addEventListener('visibilitychange',visible);
 let timer=setInterval(visible,60000);addEventListener('pagehide',()=>{clearInterval(timer);timer=null;});addEventListener('pageshow',()=>{if(timer===null)timer=setInterval(visible,60000);visible();});refresh();
 return {refresh,get status(){return current;}};
}
export function installReleaseSettings({form,api,token,el,button,isCurrent}){
 const box=el('fieldset',undefined,'settings-releases'),message=el('p','Loading release status…','settings-help');box.append(el('legend','Release notifications'),message);
 const check=button('Check now',async()=>{check.disabled=true;try{const status=await api('server-release',{admin_token:token.value});if(isCurrent())message.textContent=releaseSummary(status);}catch(error){if(isCurrent())message.textContent=error.message;}finally{check.disabled=false;}});check.type='button';box.append(check);form.append(box);
 api('server-release').then(status=>{if(isCurrent()){message.textContent=releaseSummary(status);check.disabled=status.status==='disabled';}}).catch(error=>{if(isCurrent())message.textContent=error.message;});
}
