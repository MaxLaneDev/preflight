import {installReleaseNotice} from '/releases.mjs';
import {renderEnvironment} from '/environment-editor.mjs';
import {configureLive,liveSource as sharedLiveSource} from '/live-client.mjs';
import {mountVirtualList,mountInfiniteScroll,mountVirtualCards,isPageLabel,registerPageButton,bindPageChoices} from '/virtual-list.mjs';
import {LogWindow} from '/log-window.mjs';
import {mountConsole} from '/console.mjs';
import {installExternalCI} from '/ci.mjs';
import {openDiagnostics} from '/diagnostics.mjs';
import {renderSettings} from '/settings.mjs';
import {installGlobalSearch} from '/search.mjs';
import {openRetry} from '/retry.mjs';
import {modalClose,closeDialog,installDialogInteractions,animatePage} from '/interactions.mjs';
import {msg as uiMessage,put as uiText,attr as uiAttribute,choice as uiOption,uiNode,uiError,formatted as uiFormatted,timestamp,statusLabel as uiStatus,installLocalization,cloneChoice} from '/localization.mjs';
import {installManagement} from '/management.mjs';
import {installExecutions} from '/executions.mjs';
import {installSelects} from '/selects.mjs';
import {routePath,readRoute} from '/routing.mjs';
installLocalization();
installDialogInteractions();
const $ = id => document.getElementById(id);
$('skip-content').onclick=event=>{event.preventDefault();closeMobileMenu();$('main-content').focus();};
const el = (tag, text, cls) => { const n = document.createElement(tag); if (text !== undefined) uiText(n,text); if (cls) n.className = cls; return n; };
function pipelineActionLabel(id,label){uiAttribute($(id),'aria-label',label);uiAttribute($(id),'title',label);}
function editActionLabel(label,save=false){pipelineActionLabel('edit',label);$('edit').querySelector('svg').innerHTML=save?'<path d="M4 3h13l4 4v14H3V3h1M7 3v6h10V3M7 21v-8h10v8"/>':'<path d="m16 3 5 5-12 12-6 1 1-6zM13 6l5 5"/>';}
const state = {canRetry:false,project:null,pipeline:null,definition:null,revision:0,run:null,record:null,tab:'Graph',epoch:0,historyNext:null,restoring:true,history:[],entries:[],experiments:[],section:'pipelines',stage:null,loading:false};
const retryContext=el('div',undefined,'retry-context');retryContext.hidden=true;$('run-timing').after(retryContext);
let settingsActive=false,settingsGeneration=0,ciSettingsChanged=false;let settingsContext={scope:'all'};
let codeEditor=null,editorBase="",editorBaseName="",editorGeneration=0,editorMode="code",editorModule,editorSchema;
const catalogPages={pipelines:{},experiments:{}};let catalogTimer,liveSource=null,liveKey=null,liveConnected=false,lastLiveCompletion=null,lastCatalogRefresh=0;
const syncSelects=installSelects();
let catalogList=null,scheduleList=null,scheduleItems=[],scheduleNext=null;let catalogPager=null;const navigationPagers={};let schedulesPager=null,schedulesRequest=0,schedulesLoading=false;
const initialRoute=readRoute(new URL(location.href));
installGlobalSearch({api,el,routePath,navigate:async target=>{const path=routePath(target);history.pushState(null,'',path);await restoreRoute(readRoute(new URL(path,location.origin)));}});
const terminal = new Set(['succeeded','failed','cancelled','timed_out','infrastructure_error','blocked','skipped']);
const notifications=el('section');notifications.id='notifications';notifications.popover='manual';uiAttribute(notifications,'aria-label',uiMessage("Notifications"));
document.body.append(notifications);
let notificationDialog=null;
const notificationResize=new ResizeObserver(()=>placeNotifications());
function placeNotifications(){
 const dialogs=[...document.querySelectorAll('dialog[open]')],parent=dialogs.at(-1)||document.body;
 if(notificationDialog!==dialogs.at(-1)){notificationResize.disconnect();notificationDialog=dialogs.at(-1)||null;if(notificationDialog)notificationResize.observe(notificationDialog);}
 if(notificationDialog){
  const bounds=notificationDialog.getBoundingClientRect(),actions=notificationDialog.querySelector(':scope > .actions'),edge=actions?.getBoundingClientRect().top||bounds.bottom;
  notifications.style.bottom=Math.max(12,innerHeight-edge+12)+'px';notifications.style.right=Math.max(12,innerWidth-bounds.right+12)+'px';notifications.style.maxHeight=Math.max(64,Math.min(innerHeight*.5,edge-bounds.top-70))+'px';
 }else{for(const property of ['bottom','right','max-height'])notifications.style.removeProperty(property);}
 if(notifications.parentElement!==parent){notifications.hidePopover();parent.append(notifications);}
 if(notifications.childElementCount&&!notifications.matches(':popover-open'))notifications.showPopover();
 if(!notifications.childElementCount)notifications.hidePopover();
}
window.addEventListener('resize',placeNotifications);
new MutationObserver(placeNotifications).observe(document.body,{subtree:true,attributes:true,attributeFilter:['open']});
const notice = value => {
 if(!value){notifications.querySelectorAll('.toast:not(.error)').forEach(n=>n.remove());placeNotifications();return;}
 const error=value instanceof Error,message=value?.localizedMessage||(error?value.message:value),messageText=String(message);
 for(const existing of notifications.children)if(existing.dataset.message===messageText){existing.remove();break;}
 const toast=el('div',undefined,'toast'+(error?' error':''));toast.dataset.message=messageText;
 const icon=el('span',(error?'!':'✓'),'toast-icon');icon.setAttribute('aria-hidden','true');
 const text=el('span',message,'toast-message');text.setAttribute('role',error?'alert':'status');
 const dismiss=button('×',()=>{toast.remove();placeNotifications();});uiAttribute(dismiss,'aria-label',uiMessage("Dismiss notification"));uiAttribute(dismiss,"title",uiMessage("Dismiss notification"));
 toast.append(icon,text,dismiss);notifications.append(toast);placeNotifications();
 if(!error){const expire=()=>{if(!toast.isConnected)return;if(toast.matches(':hover')||toast.contains(document.activeElement)){setTimeout(expire,1000);return;}toast.remove();placeNotifications();};setTimeout(expire,5000);}
};
const action = fn => async event => {
 if(event?.type==='submit')event.preventDefault();
 const control=event?.submitter||event?.currentTarget;
 if(control?.disabled||control?.getAttribute('aria-busy')==='true')return;
 if(control?.tagName==='BUTTON'){control.setAttribute('aria-busy','true');}
 try { await fn(event); } catch(e){notice(e);if(control?.closest?.('#editor'))notifications.lastElementChild?.setAttribute('data-context','editor');} finally {if(control?.tagName==='BUTTON'){control.removeAttribute('aria-busy');}}
};
const button = (text, fn) => {const b=el('button',text);b.type='button';b.onclick=action(isPageLabel(b.textContent)?registerPageButton(b,fn):fn);return b;};
const pretty = value => JSON.stringify(value,null,2);
const query = values => new URLSearchParams(Object.entries(values).filter(([,v])=>v!==undefined&&v!==null)).toString();
async function api(path, body, method) {const options=body===undefined?{...(method?{method}:{})}:{method:method||'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)};let r=await fetch('/api/v1/'+path,options);if(r.status===401&&state.bundled){await authenticateBrowser(true);r=await fetch('/api/v1/'+path,options);}const text=await r.text();let value;try{value=JSON.parse(text);}catch{throw uiError(text.slice(0,200));}if(!r.ok){const error=uiError((value.error?.message || pretty(value)));error.status=r.status;throw error;}return value;}
const executions=installExecutions({api,el,button,query,copy,notice,remoteChoices,openItem:openExecutionItem,onRoute:values=>{if(state.section==='executions'){const path=routePath({section:'executions',...values});history.replaceState(null,'',path);}}});
const ciPanel=el('section');ciPanel.id='ci-panel';ciPanel.hidden=true;$('explorer').append(ciPanel);
const ciNav=button('External CI',()=>showSection('ci'));const ciIcon=el('span',undefined,'section-icon');ciIcon.setAttribute('aria-hidden','true');ciIcon.innerHTML='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><path d="M3 3h6v6H3zM15 15h6v6h-6zM15 3h6v6h-6zM9 6h6M6 9v9h9"/></svg>';ciNav.prepend(ciIcon);ciNav.dataset.section='ci';ciNav.hidden=true;$('sections').append(ciNav);
const externalCI=installExternalCI({api,el,button,query,copy,notice:value=>notice(value instanceof Error?value:new Error(String(value))),target:ciPanel,openRun:item=>openExecutionItem({...item,project_id:state.project,target_kind:'run',kind:'pipeline'}),openSettings:options=>showSettings({...options,scope:'ci'}),canUse:()=>state.canCI});
const management=installManagement({api,el,button,query,notice,canRetention:()=>state.canRetention,onChanged:async({project_id,action:change,target_id,kind})=>{
 if(project_id!==state.project)return;
 if(change==='delete_pipeline'&&state.pipeline?.pipeline_key===target_id){clearPipeline();showSection('pipelines');}
 if(change==='delete_run'&&state.run===target_id){clearRun();await loadHistory(false);if(state.history.length)await selectRun(state.history[0].run_id);}
 if(change==='retention'&&state.run)await loadRun(false);
 if(change==='restore'&&kind==='run'&&state.pipeline){await loadHistory(false);if(!state.run&&state.history.some(run=>run.run_id===target_id))await selectRun(target_id);}
 await loadCatalogPage('pipelines');await loadSchedules();
}});
const managementContext=()=>({project_id:state.project,project_name:$('projects').selectedOptions[0]?.textContent});
$('manage-project').onclick=action(()=>management.open(managementContext()));
$('manage-pipeline').onclick=action(()=>management.open({...managementContext(),pipeline_key:state.pipeline?.pipeline_key,pipeline_name:state.pipeline?.name}));
$('manage-run').onclick=action(()=>{if(!state.run)throw uiError(uiMessage("Select a run."));return management.open({...managementContext(),run_id:state.run});});
$('execution-nav').onclick=()=>showExecutions();
function showExecutions(values={}){externalCI.hide();closeMobileMenu();hideSettings();state.epoch++;state.section='executions';$('explorer').hidden=true;$('run-workspace').hidden=true;$('breadcrumb').hidden=true;document.querySelector('.page-heading').hidden=true;executions.show(values);$('execution-nav').classList.add('active');updateSectionNavigation();writeRoute();}
async function openExecutionItem(item){await selectProject(item.project_id);if(item.target_kind==='run'){if(item.kind==='experiment')await selectExperiment({name:item.experiment_name});else await selectPipeline({pipeline_key:item.pipeline_key||'',name:item.name,saved_name:(item.pipeline_key||'').startsWith('saved:')?item.pipeline_key.slice(6):null});await selectRun(item.run_id||item.target_id);return;}if(item.target_kind==='schedule'){showSection('schedules');const schedule=await api('schedule?'+query({project_id:item.project_id,schedule_id:item.target_id}));await openSchedule(schedule);return;}const data=await api('change-queues/'+encodeURIComponent(item.target_id));const dialog=el('dialog');uiAttribute(dialog,'aria-label',uiMessage("Change queue"));dialog.append(el('h2',uiMessage("Change queue")),facts(data),modalClose(button(uiMessage("Close"),()=>closeDialog(dialog))));dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();}
async function copy(value){const text=typeof value==='string'?value:pretty(value);try{await navigator.clipboard.writeText(text);notice(uiMessage("Copied."));}catch{let dialog=$('copy-dialog');if(!dialog){dialog=el('dialog');dialog.id='copy-dialog';uiAttribute(dialog,'aria-label',uiMessage("Copy data"));document.body.append(dialog);}const area=el('textarea');area.value=text;area.readOnly=true;area.rows=10;uiAttribute(area,'aria-label',uiMessage("Data to copy"));dialog.replaceChildren(el('h2',uiMessage("Copy data")),el('p',uiMessage("Clipboard access is unavailable. Copy the selected text with your keyboard.")),area,modalClose(button(uiMessage("Close"),()=>closeDialog(dialog))));dialog.showModal();area.focus();area.select();}}
const copyButton = (value,label=uiMessage('Copy'))=>button(label,()=>copy(value));
function copyField(content,value,label=uiMessage('Copy')){const box=el('div',undefined,'copy-field'),control=button('⧉',()=>copy(typeof value==='function'?value():value));control.className='field-copy';uiAttribute(control,'aria-label',label);uiAttribute(control,"title",label);box.append(content,control);return box;}
function code(value){return copyField(el('pre',typeof value==='string'?value:pretty(value)),value);} 
function badge(value){return el('span',uiStatus(value||'unknown'),'badge '+(value||''));}
function detail(title,id,open=true){const d=el('details');const key='preflight:panel:'+id;try{d.open=localStorage.getItem(key)!==null?localStorage.getItem(key)==='true':open;}catch{d.open=open;}d.append(el('summary',title));d.addEventListener('toggle',()=>{try{localStorage.setItem(key,String(d.open));}catch{}});return d;}
const prefers=matchMedia('(prefers-color-scheme: dark)');
function theme(){let value=$('theme').value;document.querySelectorAll('#theme-switch button').forEach(b=>b.setAttribute('aria-pressed',String(b.dataset.theme===value)));document.documentElement.dataset.theme=value==='system'?(prefers.matches?'dark':'light'):value;try{localStorage.setItem('preflight:theme',value);}catch{}}
try{$('theme').value=localStorage.getItem('preflight:theme')||'system';}catch{}theme();$('theme').onchange=theme;prefers.addEventListener('change',theme);
const narrowNavigation=matchMedia('(max-width:600px)');
let desktopMenuHidden=false;try{desktopMenuHidden=localStorage.getItem('preflight:sidebar')==='true';}catch{}
function syncNavigation(){
 const open=narrowNavigation.matches?$('layout').classList.contains('mobile-nav-open'):!desktopMenuHidden;
 $('layout').classList.toggle('collapsed',!narrowNavigation.matches&&desktopMenuHidden);
 const toggle=$('sidebar-toggle');toggle.setAttribute('aria-expanded',String(open));uiAttribute(toggle,'aria-label',(open?uiMessage("Hide menu"):uiMessage("Show menu")));uiAttribute(toggle,"title",(open?uiMessage("Hide menu"):uiMessage("Show menu")));
 toggle.innerHTML='<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/><path d="'+(open?'m16 9-3 3 3 3':'m13 9 3 3-3 3')+'"/></svg>';
 $('sidebar').inert=!open;document.querySelector('main').inert=narrowNavigation.matches&&open;$('nav-backdrop').hidden=!(narrowNavigation.matches&&open);
}
function closeMobileMenu(){if(!$('layout').classList.contains('mobile-nav-open'))return;$('layout').classList.remove('mobile-nav-open');syncNavigation();}
$('sidebar-toggle').onclick=()=>{if(narrowNavigation.matches)$('layout').classList.toggle('mobile-nav-open');else{desktopMenuHidden=!desktopMenuHidden;try{localStorage.setItem('preflight:sidebar',String(desktopMenuHidden));}catch{}}syncNavigation();if(narrowNavigation.matches&&$('layout').classList.contains('mobile-nav-open'))$('projects-control')?.focus();};
$('nav-backdrop').onclick=closeMobileMenu;
narrowNavigation.addEventListener('change',()=>{$('layout').classList.remove('mobile-nav-open');syncNavigation();});syncNavigation();

function clearRun(){state.disposeView?.();state.disposeView=null;retryContext.hidden=true;$('manage-run').hidden=true;state.pollLogs=null;state.run=null;state.record=null;state.stage=null;state.testFilters=null;state.graphStage=null;state.viewDirty=false;$('run-panel').hidden=true;$('view').replaceChildren();if($('test-dialog').open)closeDialog($('test-dialog'));}
function clearPipeline(){state.experiment=false;state.experimentDefinition=null;state.pipeline=null;state.definition=null;state.revision=0;state.history=[];state.historyNext=null;clearRun();$('history').replaceChildren();if($('editor').open)closeDialog($('editor'));$('editor').hidden=true;$('edit').hidden=true;$('start').hidden=true;$('more-history').hidden=true;}
async function loadProjects(){
 const page=await api('catalog?'+query({kind:'workspaces',limit:25}));const projects=page.items;
 const wanted=initialRoute.get('project');if(wanted&&!projects.some(p=>p.id===wanted)){try{projects.push(await api('projects/'+encodeURIComponent(wanted)));}catch{}}
 $('projects').replaceChildren(...projects.map(p=>uiOption(p.name,p.id)));remoteChoices($('projects'),'workspaces',page.next_cursor);
 if(!projects.length){notice(uiMessage("Register a project through MCP to begin."));return;}await selectProject(projects.find(p=>p.id===(wanted||state.project))?.id||projects[0].id);
}
async function selectProject(id){window.scrollTo(0,0);notice('');state.epoch++;state.project=id;if(![...$('projects').options].some(option=>option.value===id)){const project=await api('projects/'+encodeURIComponent(id));if(state.project!==id)return;$('projects').append(uiOption(project.name,project.id));}$('projects').value=id;state.evidenceSeed=null;schedulesRequest++;schedulesLoading=false;scheduleItems=[];scheduleNext=null;schedulesPager?.destroy();schedulesPager=null;scheduleList?.destroy();scheduleList=null;clearPipeline();$('pipelines').replaceChildren();$('experiments').replaceChildren();$('schedules').replaceChildren();if($('schedule-dialog').open)closeDialog($('schedule-dialog'));uiText($('title'),uiMessage("Project pipelines"));uiText($('subtitle'),uiMessage("Select a pipeline to explore stages, runs and test results."));$('pipeline-filter').value='';$('catalog-search').value='';state.entries=[];state.experiments=[];
 catalogPages.pipelines={};catalogPages.experiments={};const epoch=state.epoch;showSection('pipelines');
 await Promise.all([loadCatalogPage('pipelines'),loadCatalogPage('experiments')]);if(state.project!==id||epoch+1!==state.epoch)return;await loadSchedules();syncLive();
}
function remoteChoices(select,kind,cursor=null){
 select.remoteController?.abort();select.remoteController=new AbortController();const signal=select.remoteController.signal;let next=cursor,term='',generation=0,timer,loading=false;
 select.dataset.remote='true';select.dataset.more=String(Boolean(next));
 async function load(reset=false){if(loading&&!reset)return;const project=state.project,request=++generation;loading=true;select.dataset.loading='true';syncSelects();
 try{const page=await api('catalog?'+query({kind,project_id:kind==='workspaces'?null:project,query:term.trim(),after:reset?null:next,limit:25}));if(signal.aborted||request!==generation||!select.isConnected||(kind!=='workspaces'&&state.project!==project))return;
 const selected=cloneChoice(select.selectedOptions[0]),value=select.value;
 if(reset)select.replaceChildren(...(selected?[selected]:[]));
 for(const row of page.items){const optionValue=kind==='workspaces'?row.id:row.saved_name||row.name;if(kind==='saved_pipelines'&&!row.saved_name)continue;let option=[...select.options].find(o=>o.value===optionValue);if(!option){option=uiOption(row.name,optionValue);select.append(option);}option.dataset.revision=row.revision||1;option.dataset.search=[row.name,row.id,row.key,row.pipeline_key,row.root].filter(Boolean).join(' ');}
 if(value)select.value=value;next=page.next_cursor;select.dataset.more=String(Boolean(next));select.dataset.error='';
 }catch(error){if(request===generation&&!signal.aborted)select.dataset.error=error.message;}finally{if(request===generation){loading=false;select.dataset.loading='false';syncSelects();}}}
 select.addEventListener('choices-search',event=>{term=event.detail;generation++;loading=false;next=null;select.dataset.more='false';select.dataset.error='';clearTimeout(timer);timer=setTimeout(()=>{if(!signal.aborted)load(true);},200);},{signal});
 select.addEventListener('choices-more',event=>event.detail?.waitUntil?.(load(!next)),{signal});
}
const pipelineListName=p=>!p.saved_name&&p.pipeline_key?.startsWith('definition:')&&p.name==='Pipeline '+p.pipeline_key.slice(11,23)?p.pipeline_key.slice(11,23):p.name;
function renderNavigation(){
 for(const [kind,rows]of [['pipelines',state.entries],['experiments',state.experiments]]){navigationPagers[kind]?.destroy();$(kind).replaceChildren(...rows.map(p=>{const name=kind==='pipelines'?pipelineListName(p):p.name,b=button(name,()=>kind==='experiments'?selectExperiment(p):selectPipeline(p));b.dataset.key=p.pipeline_key;uiAttribute(b,"title",name);return b;}));
 const page=catalogPages[kind];if(page.loading)$(kind).append(loadingIndicator('Loading…'));else {const pager=mountInfiniteScroll($('object-list'),()=>loadCatalogPage(kind,true),{host:$(kind)});navigationPagers[kind]=pager;pager.set({more:page.next,error:page.error});}}
 updateSectionNavigation();renderCatalog();filterNavigation();
}
let browserLogin=null;
async function authenticateBrowser(force=false){
 if(browserLogin)return browserLogin;
 browserLogin=(async()=>{
  if(!force){const check=await fetch('/api/v1/capabilities');if(check.ok){state.browserAuthenticated=check.headers.has('x-preflight-role');return;}if(check.status!==401)throw uiError('Unable to check API access.');}
  await new Promise(resolve=>{
   const dialog=el('dialog'),form=el('form'),label=el('label','API credential'),token=el('input'),error=el('p','','danger'),submit=el('button','Sign in');
   dialog.setAttribute('aria-label','Sign in to Preflight');token.type='password';token.autocomplete='off';token.required=true;token.minLength=32;token.maxLength=512;token.setAttribute('aria-label','API credential');submit.type='submit';submit.className='primary';error.setAttribute('role','alert');label.append(token);form.append(label,error,submit);dialog.append(el('h2','Sign in to Preflight'),el('p','Use a configured API credential. It is not stored in browser storage.','muted'),form);document.body.append(dialog);dialog.addEventListener('cancel',event=>event.preventDefault());
   form.onsubmit=async event=>{event.preventDefault();submit.disabled=true;error.textContent='';try{const response=await fetch('/api/v1/browser-session',{method:'POST',headers:{Authorization:'Bearer '+token.value}});if(!response.ok)throw Error('Sign in failed. Check your API credential.');token.value='';state.browserAuthenticated=true;dialog.close();dialog.remove();resolve();}catch(e){error.textContent=e.message;}finally{submit.disabled=false;}};
   dialog.showModal();token.focus();
  });
 })();try{await browserLogin;}finally{browserLogin=null;}
}
function loadingIndicator(text){const item=el('div',text,'loading-indicator');item.setAttribute('role','status');return item;}
async function loadCatalogPage(kind,append=false,term=$('pipeline-filter').value,refresh=false){
 const project=state.project,previous=catalogPages[kind];refresh=refresh&&Boolean(previous.loaded);if(refresh&&previous.loading)return;if(append&&(previous.loading||(!previous.next&&!previous.error)))return;
 const page=append||refresh?previous:{term,version:(previous.version||0)+1};catalogPages[kind]=page;page.loading=true;page.error=null;renderNavigation();
 try{const data=await api('catalog?'+query({kind,project_id:project,query:(page.term||'').trim(),after:append?page.next:null,limit:25}));if(state.project!==project||catalogPages[kind]!==page)return;
 const key=kind==='experiments'?'experiments':'entries';if(refresh){const last=data.items.at(-1)?.key;state[key]=[...data.items,...state[key].filter(item=>last&&item.key>last)];page.next=page.next||(data.next_cursor?state[key].at(-1)?.key:null);}else{state[key]=append&&page.loaded?[...state[key],...data.items.filter(item=>!state[key].some(old=>old.key===item.key))]:data.items;page.next=data.next_cursor;}page.loaded=true;
 }catch(error){if(state.project===project&&catalogPages[kind]===page)page.error=error.message;}
 finally{if(state.project===project&&catalogPages[kind]===page){page.loading=false;renderNavigation();}}
}
function searchCatalog(){clearTimeout(catalogTimer);const kind=state.section;if(!['pipelines','experiments'].includes(kind))return;catalogTimer=setTimeout(()=>loadCatalogPage(kind,false),250);}
$('projects').onchange=action(()=>selectProject($('projects').value));
async function selectPipeline(p){showRunWorkspace('pipelines');notice('');closeMobileMenu();state.epoch++;clearPipeline();state.pipeline=p;$('manage-pipeline').hidden=false;uiText($('start'),uiMessage("Run pipeline"));uiText($('title'),p.name);uiText($('subtitle'),(p.saved_name?uiMessage("Saved pipeline · revisioned configuration"):uiMessage("Captured pipeline definition · execution history")));document.querySelectorAll('#pipelines button').forEach(b=>b.classList.toggle('selected',b.dataset.key===p.pipeline_key));const epoch=state.epoch;
 if(p.saved_name){const cfg=await api('pipeline-configs?'+query({project_id:state.project,name:p.saved_name}));if(epoch!==state.epoch)return;state.definition=cfg.definition;state.revision=cfg.revision;$('edit').hidden=false;$('start').hidden=false;}
 await loadHistory(false);if(epoch!==state.epoch)return;
 if(state.history.length)await selectRun(state.history[0].run_id);else{$('run-panel').hidden=false;uiText($('run-title'),uiMessage("Saved definition · no runs"));uiText($('run-state'),'');$('cancel').hidden=true;$('copy-run').hidden=true;state.tab='Graph';renderTabs();await renderView();}writeRoute();
}
async function selectExperiment(p){showRunWorkspace('experiments');notice('');closeMobileMenu();state.epoch++;clearPipeline();state.experiment=true;$('manage-pipeline').hidden=true;state.pipeline={name:p.name,saved_name:p.name,pipeline_key:'experiment:'+p.name};uiText($('title'),uiMessage("Experiment · {0}",{"0":p.name}));uiText($('subtitle'),uiMessage("Experiment run history"));editActionLabel(uiMessage("Edit experiment"));uiText($('start'),uiMessage("Run experiment"));const epoch=state.epoch;
 const cfg=await api('experiment?'+query({project_id:state.project,name:p.name}));if(epoch!==state.epoch)return;state.experimentDefinition=cfg.definition;state.revision=cfg.revision;
 const validated=await api('experiments/validate',{project_id:state.project,name:p.name,expected_revision:cfg.revision,definition:cfg.definition});if(epoch!==state.epoch)return;state.definition=validated.effective_definition;$('edit').hidden=false;$('start').hidden=false;
 await loadHistory();if(epoch!==state.epoch)return;if(state.history.length)await selectRun(state.history[0].run_id);else{$('run-panel').hidden=false;uiText($('run-title'),uiMessage("Experiment revision {0} · no runs",{"0":cfg.revision}));uiText($('run-state'),'');$('cancel').hidden=true;$('copy-run').hidden=true;state.tab='Graph';renderTabs();await renderView();}writeRoute();
}
function retryStage(stageId,testId=null,testName=null){const run=state.record,project=state.project,pipelineKey=state.pipeline?.pipeline_key;return openRetry({api,el,button,notice,run,stageId,testId,testName,onStarted:async result=>{if($('test-dialog').open)await closeDialog($('test-dialog'));if(state.project!==project||state.pipeline?.pipeline_key!==pipelineKey)return;await loadHistory();await selectRun(result.id);}});}
async function loadHistory(append=false){if(!state.pipeline)return;const epoch=state.epoch;const page=await api('run-history?'+query({project_id:state.project,pipeline_key:state.experiment?null:state.pipeline.pipeline_key,experiment_name:state.experiment?state.pipeline.name:null,limit:20,before:append?state.historyNext:null}));if(epoch!==state.epoch)return;const preserve=!append&&state.history.length>20,seen=new Set(page.runs.map(row=>row.run_id));state.history=append?[...state.history,...page.runs.filter(row=>!state.history.some(old=>old.run_id===row.run_id))]:preserve?[...page.runs,...state.history.filter(row=>!seen.has(row.run_id))]:page.runs;if(!preserve)state.historyNext=page.next_cursor;$('more-history').hidden=!state.historyNext;renderHistory();}
function renderHistory(){
 const table=el('table'),head=el('tr');[uiMessage("Run"),uiMessage("Status"),uiMessage("Started")].forEach(x=>head.append(el('th',x)));table.append(head);
 const filter=$('history-filter').value;
 const runs=state.history.filter(r=>filter==='all'||(filter==='active'?!terminal.has(r.status):filter==='failed'?['failed','timed_out','infrastructure_error','blocked'].includes(r.status):r.status===filter));
 for(const r of runs){const tr=el('tr',undefined,r.run_id===state.run?'selected':'');const id=el('td'),pick=button(r.run_id.slice(-8),()=>selectRun(r.run_id));uiAttribute(pick,"title",r.run_id);uiAttribute(pick,'aria-label',uiMessage("Open run {0}",{"0":r.run_id}));id.append(pick);const status=el('td');status.append(badge(r.status));const time=el('td',timestamp(r.created_at));time.className='run-time';tr.append(id,status,time);table.append(tr);}
 $('run-picker').replaceChildren(...state.history.map(r=>uiOption(uiMessage('{0} · {1} · {2}',{0:r.run_id.slice(-8),1:uiStatus(r.status),2:timestamp(r.created_at)}),r.run_id)));if(state.run&&!state.history.some(r=>r.run_id===state.run))$('run-picker').append(uiOption(uiMessage("{0} · linked run",{"0":state.run.slice(-8)}),state.run));if(state.run)$('run-picker').value=state.run;
 $('run-picker').hidden=!$('run-picker').options.length;
 uiText($('history-count'),String(state.history.length)+(state.historyNext?'+':''));$('history').replaceChildren(table);
 if(!runs.length)$('history').append(el('p',(state.history.length?uiMessage("No runs match this filter."):uiMessage("No runs yet. Start the pipeline to see results.")),'empty-state'));
}
$('history-filter').onchange=renderHistory;$('run-picker').onchange=action(()=>selectRun($('run-picker').value));
$('pipeline-filter').oninput=()=>{filterNavigation();searchCatalog();};
$('more-history').onclick=action(registerPageButton($('more-history'),()=>loadHistory(true)));bindPageChoices($('more-history'),[$('run-picker')],{choicesOnly:false});
async function selectRun(id){state.epoch++;clearRun();state.run=id;$('manage-run').hidden=false;$('copy-run').hidden=false;$('run-panel').hidden=false;uiText($('run-title'),uiMessage("Run {0}",{"0":id.slice(-8)}));uiAttribute($('run-title'),"title",id);uiText($('view'),uiMessage("Loading run…"));renderHistory();await loadRun();writeRoute();}
async function loadRun(background=false){const epoch=state.epoch,id=state.run;if(!id)return;const run=await api('runs/'+encodeURIComponent(id));if(epoch!==state.epoch)return;if(run.project_id!==state.project)throw uiError(uiMessage("Run project mismatch"));const payloadChanged=JSON.stringify(state.record?.expired_payloads||[])!==JSON.stringify(run.expired_payloads||[]);const changed=JSON.stringify(state.record)!==JSON.stringify(run);state.viewDirty=state.viewDirty||changed;state.record=run;retryContext.replaceChildren();retryContext.hidden=!run.retry;if(run.retry)retryContext.append(el('span',uiMessage('Partial retry · {0}',{0:run.retry.stage_id})),button(uiMessage('Parent run'),()=>selectRun(run.retry.parent_run_id)));if(!state.definition)state.definition=run.pipeline||{version:1,pipeline:{stages:run.stages.map(s=>s.definition)}};if(state.definition){$('edit').hidden=!!run.external;editActionLabel((state.experiment?uiMessage("Edit experiment"):(state.pipeline?.saved_name?uiMessage("Edit pipeline"):uiMessage("Save as pipeline"))),!state.experiment&&!state.pipeline?.saved_name);}$('run-state').replaceChildren(badge(run.status));$('run-timing').replaceChildren(el('span','Started · '+timestamp(run.created_at)),el('span','Finished · '+timestamp(run.finished_at)),el('span',duration((run.finished_at||Date.now())-run.created_at)+(run.finished_at?' elapsed':' elapsed · updating')));uiAttribute($('run-timing'),'title','Local time · '+Intl.DateTimeFormat().resolvedOptions().timeZone);$('cancel').hidden=terminal.has(run.status);if(!background||payloadChanged||state.viewDirty&&!['Logs','Paths','Changes'].includes(state.tab)&&!$('view').contains(document.activeElement)){state.viewDirty=false;renderTabs();await renderView();}}
$('copy-run').onclick=action(()=>copy(state.run));
$('cancel').onclick=action(async()=>{const id=state.run;if(!id)return;await api('runs/'+encodeURIComponent(id)+'/cancel',{actor:'user',reason:'Cancelled from Preflight client'});if(state.run===id){notice(uiMessage("Cancellation requested; waiting for terminal state."));await loadRun();}});
const tabs=['Overview','Graph','Tests','Acceptance','Logs','Artifacts','Changes','Paths'];
function renderTabs(){
 const restoreFocus=$('tabs').contains(document.activeElement);
 $('tabs').setAttribute('role','tablist');
 $('tabs').replaceChildren(...tabs.map(tab=>{const selected=state.tab===tab,b=button(uiMessage(tab),async()=>{state.tab=tab;renderTabs();await renderView();writeRoute();});
 b.id='tab-'+tab.toLowerCase();b.setAttribute('role','tab');b.setAttribute('aria-selected',String(selected));b.setAttribute('aria-controls','view');b.tabIndex=selected?0:-1;b.classList.toggle('active',selected);
 b.onkeydown=event=>{let index=tabs.indexOf(tab);if(event.key==='ArrowRight')index=(index+1)%tabs.length;else if(event.key==='ArrowLeft')index=(index+tabs.length-1)%tabs.length;else if(event.key==='Home')index=0;else if(event.key==='End')index=tabs.length-1;else return;event.preventDefault();const next=$('tabs').children[index];next.focus();next.click();};return b;}));
 $('view').setAttribute('role','tabpanel');$('view').setAttribute('aria-labelledby','tab-'+state.tab.toLowerCase());
 if(restoreFocus)$('tab-'+state.tab.toLowerCase()).focus({preventScroll:true});requestAnimationFrame(revealSelectedTab);
}

export function pipelineLevels(stages){const byId=new Map(stages.map(s=>[s.id,s]));const levels=new Map(),active=new Set();function visit(id){if(levels.has(id))return levels.get(id);if(active.has(id))throw uiError(uiMessage("Pipeline cycle"));active.add(id);const s=byId.get(id);if(!s)throw uiError(uiMessage("Unknown dependency {0}",{"0":id}));let level=0;for(const dep of s.needs||[])level=Math.max(level,visit(dep)+1);active.delete(id);levels.set(id,level);return level;}stages.forEach(s=>visit(s.id));return levels;}
function graph(target,definition,results=[]){
 target.replaceChildren();const imported=target.id!=='editor-graph'&&!!state.record?.external;const stages=definition?.pipeline?.stages||[];
 if(!stages.length){target.append(el('p',uiMessage("No DAG definition available."),'empty-state'));return;}
 const tools=el('div',undefined,'graph-toolbar'),heading=el('div');heading.append(el('strong',imported?'Imported jobs':uiMessage("Pipeline graph")),el('span',imported?'Read-only · provider dependencies unavailable':uiMessage("{0} stages · select a block for details",{"0":stages.length}),'muted'));
 const zoom=el('input');zoom.type='range';zoom.min='20';zoom.max='140';zoom.value='100';uiAttribute(zoom,'aria-label',uiMessage("Graph zoom"));const zoomLabel=el('span','100%');
 const zoomOut=button('−',()=>setZoom(Number(zoom.value)-10)),zoomIn=button('+',()=>setZoom(Number(zoom.value)+10));zoomOut.className=zoomIn.className='graph-zoom-button';
 const fitButton=button('',()=>fit()),resetButton=button('',()=>{setZoom(100);viewport.scrollTo(0,0);});
 for(const [control,label,path]of [[fitButton,'Fit to screen','M8 3H3v5m13-5h5v5M3 16v5h5m13-5v5h-5M8 8h8v8H8z'],[resetButton,'Reset view','M3 10a9 9 0 1 1 2 8M3 4v6h6']]){control.className='graph-zoom-button graph-icon';uiAttribute(control,'aria-label',uiMessage(label));uiAttribute(control,'title',uiMessage(label));control.innerHTML=`<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="${path}"/></svg>`;}
 tools.append(heading,el('span',undefined,'spacer'),zoomOut,zoom,zoomIn,zoomLabel,fitButton,resetButton);
 const workspace=el('div',undefined,'graph-workspace'),viewport=el('div',undefined,'graph-viewport'),dag=el('div',undefined,'dag'),inspector=el('section',undefined,'stage-inspector');inspector.hidden=true;
 const levels=pipelineLevels(stages),columns=new Map(),nodes=new Map();
 function inspect(s){state.graphStage=s.id;nodes.forEach((n,id)=>n.classList.toggle('selected',id===s.id));inspector.hidden=false;inspector.replaceChildren();const result=results.find(r=>r.definition.id===s.id),top=el('div',undefined,'heading');top.append(el('h3',result?.external?.name||s.id),button(uiMessage("Close details"),()=>{state.graphStage=null;inspector.hidden=true;nodes.forEach(n=>n.classList.remove('selected'));drawEdges();}));inspector.append(top,badge(result?.status||'not_run'));const command=s.command?[s.command.program,...s.command.args||[]]:'Fixture stage';if(!imported)inspector.append(el('h3',uiMessage("Command")),code(command));else inspector.append(code(result.external),el('p','Provider job · dependencies and source workspace unavailable.','muted'));const info=el('dl');for(const [label,value]of [[uiMessage("Depends on"),imported?'Unavailable':(s.needs||[]).join(', ')||uiMessage('No dependencies')],[uiMessage("Working directory"),imported?'Unavailable':s.cwd||'.'],[uiMessage("Timeout"),!imported&&s.command?.timeout_seconds?uiMessage('{0} seconds',{0:s.command.timeout_seconds}):'—'],[uiMessage("Result"),result?.reason||uiStatus(result?.status)||uiMessage('Not started')]]){info.append(el('dt',label),el('dd',value));}inspector.append(info);if(state.run&&target.id!=='editor-graph'){const actions=el('div',undefined,'actions');actions.append(button(uiMessage("Open stage logs"),async()=>{state.stage=s.id;state.tab='Logs';renderTabs();await renderView();}),button(uiMessage("View tests"),async()=>{state.tab='Tests';renderTabs();await renderView();}));if(state.canRetry&&!state.record?.external&&terminal.has(state.record?.status))actions.append(button(uiMessage('Retry stage'),()=>retryStage(s.id)));inspector.append(actions);}const raw=detail(imported?'Provider metadata':uiMessage("Stage definition"),'stage-definition',false);raw.append(code(imported?result?.external||{stage_id:s.id,kind:'imported summary'}:s));inspector.append(raw);drawEdges();}
 for(const s of stages){const level=levels.get(s.id);if(!columns.has(level)){const c=el('div',undefined,'dag-column');c.append(el('div',(level===0?uiMessage("Start"):uiMessage("Step {0}",{"0":level+1})),'column-label'));columns.set(level,c);}const result=results.find(r=>r.definition.id===s.id),node=el('div',undefined,'stage '+(result?.status||''));const select=button(result?.external?.name||s.id,()=>inspect(s));select.className='stage-select';const command=el('div',imported?(s.id==='ci-tests'?'Published tests':s.id==='ci-summary'?'Workflow result':'Provider job'):(s.kind==='test'?testKindLabel(s.testing?.kind||(s.fixtures?'fixture':'custom'))+' · ':'')+(s.command?.program||'Fixtures'),'stage-command');uiAttribute(command,"title",(imported?'Imported evidence; no native command':s.command?JSON.stringify([s.command.program,...s.command.args||[]]):uiMessage("Fixture tests")));node.append(select,badge(result?.status||'not_run'),command);node.onclick=event=>{if(event.target===node)inspect(s);};columns.get(level).append(node);nodes.set(s.id,node);}
 for(const [,c]of [...columns].sort((a,b)=>a[0]-b[0])){if(c.querySelectorAll('.stage').length>4)c.classList.add('parallel-grid');dag.append(c);}viewport.append(dag);workspace.append(viewport,inspector);target.append(tools,workspace);
 function setZoom(value){zoom.value=String(Math.max(20,Math.min(140,value)));dag.style.zoom=Number(zoom.value)/100;uiText(zoomLabel,zoom.value+'%');drawEdges();}zoom.oninput=()=>setZoom(Number(zoom.value));tools.querySelectorAll('button').forEach((b,index)=>{if(index<2){uiAttribute(b,'aria-label',(index?uiMessage("Zoom in"):uiMessage("Zoom out")));uiAttribute(b,"title",(index?uiMessage("Zoom in"):uiMessage("Zoom out")));}});
 function drawEdges(){requestAnimationFrame(()=>{if(!dag.isConnected)return;dag.querySelector('.edges')?.remove();const ns='http://www.w3.org/2000/svg',svg=document.createElementNS(ns,'svg'),scale=Number(zoom.value)/100;svg.setAttribute('class','edges');svg.setAttribute('width',dag.scrollWidth);svg.setAttribute('height',dag.scrollHeight);svg.setAttribute('aria-hidden','true');const base=dag.getBoundingClientRect();for(const s of stages)for(const dep of s.needs||[]){const a=nodes.get(dep).getBoundingClientRect(),b=nodes.get(s.id).getBoundingClientRect(),line=document.createElementNS(ns,'path');const x1=(a.right-base.left)/scale,y1=(a.top-base.top+a.height/2)/scale,x2=(b.left-base.left)/scale,y2=(b.top-base.top+b.height/2)/scale;line.setAttribute('d',`M${x1} ${y1} C${x1+24} ${y1} ${x2-24} ${y2} ${x2} ${y2}`);svg.append(line);}dag.prepend(svg);});}
 let drag=null;viewport.onpointerdown=e=>{if(e.target.closest('button,input,.stage'))return;drag={x:e.clientX,y:e.clientY,left:viewport.scrollLeft,top:viewport.scrollTop};viewport.setPointerCapture(e.pointerId);};viewport.onpointermove=e=>{if(drag){viewport.scrollLeft=drag.left+drag.x-e.clientX;viewport.scrollTop=drag.top+drag.y-e.clientY;}};viewport.onpointerup=viewport.onpointercancel=()=>drag=null;
 function fit(){dag.style.zoom=1;const width=dag.scrollWidth,height=dag.scrollHeight;setZoom(Math.floor(Math.min(1,(viewport.clientWidth-4)/width,((target.id==='view'?viewport.clientHeight:Math.min(innerHeight*.64,620))-4)/height)*100));viewport.scrollTo(0,0);}
 requestAnimationFrame(()=>{if(state.graphStage&&nodes.has(state.graphStage))inspect(stages.find(s=>s.id===state.graphStage));fit();});
}
async function renderView(){
 const epoch=state.epoch,tab=state.tab;
 try{await renderViewContent();}catch(error){if(epoch!==state.epoch||tab!==state.tab)return;const box=el('div',undefined,'empty-state');box.setAttribute('role','alert');box.append(el('strong',uiMessage("Unable to load {0}",{"0":tab.toLowerCase()})),el('p',error.message),button(uiMessage("Retry"),renderView));$('view').replaceChildren(box);}
}
async function renderViewContent(){state.disposeView?.();state.disposeView=null;state.pollLogs=null;const epoch=state.epoch,tab=state.tab,id=state.run,run=state.record;writeRoute();const target=$('view');target.classList.toggle('graph-view',tab==='Graph');target.classList.toggle('logs-view',tab==='Logs');target.classList.remove('console-expanded');target.replaceChildren();if(tab==='Graph'){graph(target,run?.pipeline||state.definition||(run?{pipeline:{stages:run.stages.map(s=>s.definition)}}:null),run?.stages||[]);return;}if(!run){target.append(el('p',uiMessage("Select a run to inspect its evidence.")));return;}
 if(run.external){const provenance=el('div',undefined,'ci-provenance');provenance.append(el('strong','Imported CI results · read-only'),el('span','Local source snapshot and DAG dependencies were not imported.'),button('Inspect import',()=>externalCI.inspect(run.project_id,id)));target.append(provenance);}
 if(tab==='Artifacts'&&run.external){await externalCI.renderArtifacts(target,run.project_id,id,()=>epoch===state.epoch&&state.tab===tab);return;}
 if(tab==='Paths'&&run.external){target.append(el('h3','Imported evidence'),code({workspace:run.workspace,provider:run.external}),el('p','Provider paths are not local source or stage working directories.','muted'));return;}
 if(tab==='Tests'&&run.external&&!run.stages.some(s=>s.tests)){target.append(el('p','Structured test results unavailable. Job success does not establish test counts.','empty-state'));return;}
 if(tab==='Overview'){
 const completed=run.stages.filter(s=>terminal.has(s.status)).length;
 const summary=el('div',undefined,'run-summary');for(const [label,value]of [[uiMessage("Result"),uiStatus(run.status)],[uiMessage("Stages complete"),completed+' / '+run.stages.length],[uiMessage("Duration"),duration((run.finished_at||Date.now())-run.created_at)]]){const card=el('div',undefined,'summary-card');card.append(el('span',label,'muted'),el('strong',value));summary.append(card);}target.append(summary);
 const progress=el('progress');progress.max=run.stages.length||1;progress.value=completed;uiAttribute(progress,'aria-label',uiMessage("Completed stages"));progress.className='stage-progress';target.append(progress);
 if(run.reason)target.append(el('p',run.reason,'failure-summary'));
 const heading=el('div',undefined,'heading');heading.append(el('h3',uiMessage("Stages")),button(uiMessage("Open pipeline graph"),async()=>{state.tab='Graph';renderTabs();await renderView();}));target.append(heading);
 const table=el('table',undefined,'stage-table'),head=el('tr');for(const name of [uiMessage("Stage"),uiMessage("Status"),uiMessage("Duration"),uiMessage("Dependencies"),''])head.append(el('th',name));table.append(head);
 for(const s of run.stages){const row=el('tr'),name=el('td'),status=el('td'),actions=el('td');name.append(button(s.external?.name||s.definition.id,async()=>{state.graphStage=s.definition.id;state.tab='Graph';renderTabs();await renderView();}));status.append(badge(s.status));actions.append(button(uiMessage("Logs"),async()=>{state.stage=s.definition.id;state.tab='Logs';renderTabs();await renderView();}));if(state.canRetry&&!state.record?.external&&terminal.has(run.status))actions.append(button(uiMessage('Retry stage'),()=>retryStage(s.definition.id)));row.append(name,status,el('td',(s.started_at?duration((s.finished_at||Date.now())-s.started_at):'—')),el('td',((s.definition.needs||[]).join(', ') || uiMessage("Start")),'muted'),actions);table.append(row);}const scroll=el('div',undefined,'table-scroll');scroll.append(table);target.append(scroll);
 const metadata=detail(uiMessage("Run identifiers and metadata"),'run-metadata',false);metadata.append(code({run_id:id,project_id:run.project_id,started_at:run.created_at,finished_at:run.finished_at,cancel_actor:run.cancel_actor,reason:run.reason}));target.append(metadata);return;
 }
 const base='runs/'+encodeURIComponent(id);
 if(tab==='Tests'){await renderTests(target,epoch,id);return;}
 if(tab==='Acceptance'){await renderAcceptance(target,id,epoch);return;}
 if(tab==='Logs'&&run.expired_payloads?.includes('logs')){target.append(el('div','Log payloads expired. Run results and event metadata are retained.','empty-state'));return;}
 if(tab==='Logs'){const controls=el('div',undefined,'log-controls'),select=el('select');uiAttribute(select,'aria-label',uiMessage("Log stage"));select.append(uiOption(uiMessage("All stages"),''));for(const s of run.stages)select.append(uiOption(s.external?.name||s.definition.id,s.definition.id));select.value=state.stage||'';select.onchange=action(async()=>{state.stage=select.value||null;await renderView();});controls.append(select);const filters=button(uiMessage("Filters"),()=>{const open=controls.classList.toggle('filters-open');filters.setAttribute('aria-expanded',String(open));if(open)search.focus();});filters.className='log-filter-toggle';filters.setAttribute('aria-expanded','false');controls.append(filters);const search=el('input');search.type='search';uiAttribute(search,"placeholder",uiMessage("Search this run’s logs"));uiAttribute(search,'aria-label',uiMessage("Search logs"));const hits=el('div',undefined,'log-search-results');let logSearchGeneration=0;const searchButton=button(uiMessage("Search"),async()=>{
 const term=search.value.trim(),request=++logSearchGeneration,selectedStage=state.stage;if(!term){hits.replaceChildren();search.focus();return;}let before=null;hits.replaceChildren();const more=button(uiMessage("More matches"),async()=>{const result=await api('evidence/search?'+query({project_id:state.project,run_id:id,stage_id:selectedStage,query:term,before,limit:20}));if(epoch!==state.epoch||state.tab!==tab||request!==logSearchGeneration)return;for(const match of result.matches)hits.insertBefore(evidenceCard(match),more);before=result.next_cursor;more.hidden=!before;if(!result.matches.length&&hits.childElementCount===1)hits.prepend(el('p',uiMessage("No matches."),'muted'));});hits.append(more);await more.onclick();
 });controls.append(search,searchButton,button(uiMessage("Evidence search"),()=>openEvidence({run_id:id,stage_id:state.stage,query:search.value})));
 search.oninput=()=>{logSearchGeneration++;hits.replaceChildren();};search.onkeydown=event=>{if(event.key==='Enter'){event.preventDefault();searchButton.click();}};target.append(controls,hits);let follow=true;const stage=state.stage,pre=el('div','','log-output'),status=el('span',uiMessage("Connecting…"),'muted');pre.tabIndex=0;pre.setAttribute('role','log');pre.setAttribute('aria-live','off');uiAttribute(pre,'aria-label',uiMessage("Run console"));status.setAttribute('role','status');
 const following=button(uiMessage("Following output"),()=>{follow=!follow;if(follow)readLogs('tail');syncFollow();});
 function syncFollow(){const label=follow?uiMessage("Following output"):uiMessage("Resume following");uiAttribute(following,'aria-label',label);uiAttribute(following,'title',label);following.setAttribute('aria-pressed',String(follow));}
 pre.addEventListener('scroll',()=>{if(reading)return;follow=!window.newer&&pre.scrollHeight-pre.scrollTop-pre.clientHeight<40;syncFollow();if(!follow&&pre.scrollTop<160&&window.older)readLogs('older');else if(pre.scrollHeight-pre.scrollTop-pre.clientHeight<160&&window.newer)readLogs('newer');},{passive:true});
 pre.addEventListener('wheel',event=>{if(event.deltaY<0&&pre.scrollTop<160&&window.older&&!reading){follow=false;readLogs('older');}},{passive:true});
 const icon=(control,paths,label)=>{control.classList.add('console-icon');control.innerHTML='<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">'+paths+'</svg>';uiAttribute(control,'aria-label',label);uiAttribute(control,'title',label);return control;};
 const expandPath='<path d="M8 3H3v5m13-5h5v5M3 16v5h5m8 0h5v-5"/>',restorePath='<path d="M3 8h5V3m8 0v5h5M3 16h5v5m8 0v-5h5"/>';
 const logTools=el('div',undefined,'actions log-tools');const expand=button('',()=>{const open=target.classList.toggle('console-expanded');icon(expand,open?restorePath:expandPath,open?uiMessage('Restore console layout'):uiMessage('Expand console'));expand.setAttribute('aria-pressed',String(open));});icon(expand,expandPath,uiMessage('Expand console'));expand.setAttribute('aria-pressed','false');
 const wrap=icon(button('',()=>{pre.classList.toggle('nowrap');wrap.setAttribute('aria-pressed',String(!pre.classList.contains('nowrap')));}),'<path d="M3 6h18M3 12h12a3 3 0 1 1 0 6h-3m3-3-3 3 3 3M3 18h4"/>',uiMessage('Wrap lines'));
 icon(following,'<circle cx="12" cy="12" r="9"/><path d="M12 6v12m-4-4 4 4 4-4"/>',uiMessage('Following output'));
 const jump=icon(button('',()=>{follow=true;readLogs('tail');syncFollow();}),'<path d="M12 4v12m-5-5 5 5 5-5M5 21h14"/>',uiMessage('Jump to end'));const renderer=mountConsole(pre,{copyCode:code,virtual:true});const window=new LogWindow();state.disposeView=()=>renderer.destroy();let raw=false;const rawToggle=icon(button('',()=>{const scroll=pre.scrollTop;raw=!raw;renderer.setRaw(raw);rawToggle.setAttribute('aria-pressed',String(raw));pre.scrollTop=follow?pre.scrollHeight:scroll;}),'<path d="m8 7-5 5 5 5m8-10 5 5-5 5m-3-13-2 16"/>','Raw output');rawToggle.setAttribute('aria-pressed','false');logTools.append(expand,wrap,following,jump,rawToggle,status);
 const retained=el('span','','muted');retained.setAttribute('role','status');const field=copyField(pre,()=>renderer.rawText(),uiMessage("Copy logs"));field.classList.add('console-field');target.append(logTools,retained,field);wrap.setAttribute('aria-pressed','true');syncFollow();
 const current=()=>pre.isConnected&&epoch===state.epoch&&state.tab===tab&&state.run===id;
 let reading=false,initialized=false,timer;pre.style.opacity='0';
 const dispose=state.disposeView;state.disposeView=()=>{clearTimeout(timer);dispose();};
 async function previous(before){
  const index=await api('evidence/search?'+query({project_id:run.project_id,run_id:id,stage_id:stage,before,limit:16}));
  if(!index.matches.length)return {events:[],older:false};
  const first=index.matches.at(-1).cursor;
  const page=await api(base+'/logs?'+query({after:first-1,stage}));
  return {events:page.events.filter(e=>!before||e.id<before),older:index.next_cursor!==null};
 }
 async function readLogs(direction='poll'){
  if(reading||!current())return;reading=true;clearTimeout(timer);
  try{
   if(document.hidden&&initialized)return;
   if(!initialized||direction==='tail'){
    uiText(status,'Loading latest output…');
    const page=await previous();if(!current())return;
    window.replace(page.events,page.older);initialized=true;follow=true;
   }else if(direction==='older'){
    if(!window.older)return;uiText(status,'Loading earlier output…');
    const page=await previous(window.first);if(!current())return;window.prepend(page.events,page.older);follow=false;
   }else{
    if(direction==='poll'&&!follow)return;
    const page=await api(base+'/logs?'+query({after:window.last,stage}));if(!current())return;
    if(!page.events.length){if(terminal.has(state.record?.status))renderer.replace(window.events,{end:follow,complete:true});uiText(status,terminal.has(state.record?.status)?'End of log':'Live output');return;}
    window.append(page.events,page.next_cursor!==null);
   }
   const complete=terminal.has(state.record?.status)&&!window.newer;
   const count=renderer.replace(window.events,{end:follow,complete});
   uiText(retained,count+' loaded lines · scroll for more');
   uiText(status,window.newer?'Earlier output':complete?'End of log':'Live output');
   await new Promise(resolve=>requestAnimationFrame(resolve));if(!current())return;if(follow)renderer.end();pre.style.opacity='';syncFollow();
  }catch(error){if(!current())return;uiText(status,'Could not load output');uiAttribute(status,'title',error.message);pre.style.opacity='';}
  finally{reading=false;if(current()&&!terminal.has(state.record?.status))timer=setTimeout(()=>readLogs(),liveConnected?10000:1000);}
 }
 state.pollLogs=readLogs;
 await readLogs();return;}
 if(tab==='Artifacts'){const artifacts=await api(base+'/artifacts');if(epoch!==state.epoch||state.tab!==tab)return;target.append(el('h3',uiMessage("Published artifacts")));for(const a of artifacts){const card=el('article',undefined,'artifact-card'),head=el('div',undefined,'heading');head.append(el('strong',a.name),badge(a.availability||'available'));card.append(head,el('p',a.stage_id+' · '+formatBytes(a.size),'muted'));pathRow(card,uiMessage("File"),a.path);pathRow(card,'SHA-256',a.sha256);const d=detail(a.stage_id+' / '+a.name+(a.availability&&a.availability!=='available'?' · '+a.availability:''),`${id}:artifact:${a.name}`,false);d.append(code(a));card.append(d);target.append(card);}if(!artifacts.length)target.append(el('p',uiMessage("This run has no published artifacts."),'empty-state'));return;}
 if(tab==='Changes'){
  const source=await api('run-source?'+query({run_id:id}));if(epoch!==state.epoch||state.tab!==tab)return;
  await renderSourceChanges(target,source,id,epoch);const identity=detail('Source identity and hashes','source-identity',false);identity.append(facts(source));target.append(identity);
  await renderRunComparison(target,id,epoch);return;
 }
 if(tab==='Paths'){
 if(run.expired_payloads?.includes('source_snapshots'))target.append(el('p','Source snapshot expired. Recorded paths and SHA metadata remain available.','muted'));
 target.append(el('h3',uiMessage("Run paths")));pathRow(target,uiMessage("Workspace"),run.workspace);pathRow(target,uiMessage("Captured source"),run.pipeline?run.workspace+'/source':run.workspace);
 for(const s of run.stages){const stage=detail(uiMessage("{0} · paths",{"0":s.definition.id}),`${id}:paths:${s.definition.id}`,false);pathRow(stage,uiMessage("Workspace"),run.pipeline?run.workspace+'/stages/'+s.definition.id:run.workspace);pathRow(stage,uiMessage("Working directory"),s.definition.cwd||'.');for(const [name,output]of Object.entries(s.definition.outputs||{}))pathRow(stage,uiMessage('Output · {0}',{0:name}),output.path);target.append(stage);}
 const list=el('div',undefined,'input-paths-window'),progress=el('p','Loading paths…','muted');list.tabIndex=0;list.setAttribute('role','region');list.setAttribute('aria-label','Captured input files');progress.setAttribute('role','status');
 target.append(el('h3','Captured input files'),list,progress);
 const entries=[],paths=mountVirtualList(list,{key:item=>item.path,estimate:72,render:item=>{const wrapper=el('div');pathRow(wrapper,item.path,item.metadata.sha256||item.metadata.kind);return wrapper;}});state.disposeView=()=>paths.destroy();
 let offset=0,loading=false,more=true;
 const retry=button('Retry',()=>loadPaths());retry.hidden=true;target.append(retry);
 async function loadPaths(){
  if(loading||!more||!list.isConnected)return;loading=true;retry.hidden=true;progress.textContent='Loading paths…';list.setAttribute('aria-busy','true');
  try{const page=await api('run-inputs?'+query({run_id:id,offset}));if(epoch!==state.epoch||state.tab!==tab||!list.isConnected)return;
   entries.push(...page.manifest?.entries||[]);paths.setItems(entries);offset=page.manifest?.next_cursor;more=offset!==null&&offset!==undefined;
   progress.textContent=page.manifest?entries.length+' / '+page.manifest.total+' paths': 'No captured input manifest is available.';
  }catch(error){progress.textContent=error.message;retry.hidden=false;}finally{loading=false;list.setAttribute('aria-busy','false');}
 }
 list.addEventListener('scroll',()=>{if(list.scrollHeight-list.scrollTop-list.clientHeight<250)loadPaths();},{passive:true});
 await loadPaths();
 for(const [label,path]of [[uiMessage("Prepared modules"),'run-module-inputs'],[uiMessage("Compiler and SDK"),'run-toolchain']]){
 const d=detail(label,`${id}:${path}`,false),body=el('div');let loaded=false,loading=false;d.append(body);target.append(d);
 const load=async()=>{if(loaded||loading||!d.isConnected)return;loading=true;body.replaceChildren(el('p',uiMessage("Loading…"),'muted'));try{const page=await api(path+'?'+query({run_id:id}));if(epoch!==state.epoch||state.tab!==tab||!d.isConnected)return;body.replaceChildren(path==='run-module-inputs'&&page.entries?.length===0?el('p',uiMessage("No prepared modules."),'muted'):path==='run-toolchain'&&!page.verified?el('p',uiMessage("No pinned compiler or SDK."),'muted'):facts(page));loaded=true;}catch(error){body.replaceChildren(el('p',error.message,'failed'),button(uiMessage("Retry"),load));}finally{loading=false;}};
 d.addEventListener('toggle',()=>{if(d.open)load();});if(d.open)await load();}return;
 }
}
const testKinds=[['unit',uiMessage("Unit tests")],['integration',uiMessage("Integration tests")],['functional',uiMessage("Functional tests")],['fuzz',uiMessage("Fuzzing")],['fixture',uiMessage("Fixtures")],['custom',uiMessage("Other tests")]];
const testKindLabel=value=>testKinds.find(([key])=>key===value)?.[1]||value;
async function renderTests(target,epoch,id){
 const toolbar=el('div',undefined,'test-toolbar'),search=el('input'),status=el('select'),kind=el('select'),tree=el('div',undefined,'test-tree'),summary=el('div',undefined,'test-summary'),empty=el('p',uiMessage("Loading tests…"),'empty-state');
 search.type='search';uiAttribute(search,"placeholder",uiMessage("Find a test…"));uiAttribute(search,'aria-label',uiMessage("Find a test"));uiAttribute(status,'aria-label',uiMessage("Test status"));uiAttribute(kind,'aria-label',uiMessage("Test type"));
 for(const [value,label]of [['all',uiMessage("All statuses")],['failed',uiMessage("Failed")],['passed',uiMessage("Passed")],['skipped',uiMessage("Skipped")],['running',uiMessage("Running")]])status.append(uiOption(label,value));kind.append(uiOption(uiMessage("All types"),''));for(const [value,label]of testKinds)kind.append(uiOption(label,value));
 search.value=state.testFilters?.search||'';status.value=state.testFilters?.status||'all';kind.value=state.testFilters?.kind||'';let loaded=0,total=0,offset=0;const groups=new Map();
 const filter=()=>{state.testFilters={search:search.value,status:status.value,kind:kind.value};let shown=0;for(const row of tree.querySelectorAll('.tree-row')){row.hidden=!(`${row.textContent} ${row.dataset.search||row.dataset.testId||''}`.toLowerCase().includes(search.value.trim().toLowerCase())&&(status.value==='all'||row.dataset.status===status.value)&&(!kind.value||row.dataset.kind===kind.value));if(!row.hidden)shown++;}for(const group of tree.querySelectorAll('details')){const rows=[...group.querySelectorAll('.tree-row')],visible=rows.filter(r=>!r.hidden);group.hidden=!visible.length;uiText(group.querySelector('.tree-count'),uiMessage("{0} loaded",{"0":visible.length}));if(search.value&&visible.length)group.open=true;}empty.hidden=shown>0;uiText(empty,(loaded?uiMessage("No tests match these filters."):(!terminal.has(state.record?.status)?uiMessage("Waiting for test results…"):(state.record?.stages.some(s=>s.definition.kind==='test')?uiMessage("Individual test results were not recorded."):uiMessage("No tests in this run.")))));toolbar.hidden=!loaded&&!search.value&&status.value==='all'&&!kind.value;};
 let testsTimer;function refilter(){filter();if(!state.canTestSearch)return;testsGeneration++;clearTimeout(testsTimer);next.hidden=true;testsTimer=setTimeout(()=>{if(epoch!==state.epoch||state.tab!=='Tests'||!tree.isConnected)return;loaded=0;offset=0;groups.clear();tree.replaceChildren();empty.hidden=false;uiText(empty,'Searching…');loadTests().catch(notice);},200);}search.oninput=refilter;status.onchange=refilter;kind.onchange=refilter;toolbar.append(search,status,kind,button(uiMessage("Expand all"),()=>tree.querySelectorAll('details').forEach(d=>d.open=true)),button(uiMessage("Collapse all"),()=>tree.querySelectorAll('details').forEach(d=>d.open=false)));toolbar.children[3].classList.add('test-expand');toolbar.children[4].classList.add('test-collapse');for(const b of [toolbar.children[3],toolbar.children[4]]){uiAttribute(b,'aria-label',b.textContent);uiAttribute(b,"title",b.textContent);}target.append(toolbar,summary,tree,empty);
 let testsGeneration=0;async function loadTests(){const generation=testsGeneration;const page=await api('test-results?'+query({project_id:state.project,run_id:id,offset,...(state.canTestSearch?{query:search.value.trim(),status:status.value,kind:kind.value}: {})}));if(epoch!==state.epoch||state.tab!=='Tests'||generation!==testsGeneration)return;const totals=new Map();for(const item of page.stages.filter(s=>s.counts)){const type=item.kind||'custom',counts=totals.get(type)||{passed:0,failed:0,skipped:0};for(const key of ['passed','failed','skipped'])counts[key]+=item.counts[key];totals.set(type,counts);}summary.replaceChildren(...[...totals].map(([type,c])=>el('p',uiMessage("{0} · {1} passed · {2} failed · {3} skipped",{"0":testKindLabel(type),"1":c.passed,"2":c.failed,"3":c.skipped}),'muted')),...page.stages.filter(s=>s.counts&&!s.cases_complete).map(s=>el('p',uiMessage("{0} · incomplete case evidence",{"0":s.stage_id}),'muted')));total=page.total;
 for(const item of page.cases){const c=item.case,type=item.kind||'custom',key=type+':'+item.stage_id+':'+(c.suite||c.class_name||'Tests');if(!groups.has(key)){const group=detail(testKindLabel(type)+' / '+item.stage_id+((c.suite||c.class_name)&&c.suite!==item.stage_id?' / '+(c.suite||c.class_name):''),`${id}:tests:${key}`,true);group.querySelector('summary').append(el('span','','tree-count'));tree.append(group);groups.set(key,group);}const row=el('div',undefined,'tree-row');row.dataset.status=c.status;row.dataset.kind=type;row.dataset.testId=c.id;row.dataset.search=[c.name,c.id,c.suite,c.class_name,item.stage_id].filter(Boolean).join(' ');row.append(badge(c.status),button(c.name,()=>openTestCase(id,item,epoch)),button(uiMessage("Logs"),()=>openEvidence({run_id:id,stage_id:item.stage_id,test_id:c.id,test_name:c.name})));groups.get(key).append(row);loaded++;if(![...kind.options].some(o=>o.value===type))kind.append(uiOption(testKindLabel(type),type));}
 offset=page.next_cursor;next.hidden=offset===null;uiText(next,uiMessage("More tests · {0} / {1}",{"0":loaded,"1":total}));filter();}const next=button(uiMessage("More tests"),loadTests);target.append(next);await next.onclick();if(!loaded&&terminal.has(state.record?.status)&&state.record?.stages.some(s=>s.definition.kind==='test')){target.append(button(uiMessage("Configure reports for future runs"),()=>openEditor()));}
}
async function openTestCase(id,item,epoch){
 if(!item.native_fixture){await showTest(id,item,epoch);return;}
 const result=await api('runs/'+encodeURIComponent(id)+'/fixtures/'+encodeURIComponent(item.stage_id)+'/'+encodeURIComponent(item.case.id));if(epoch!==state.epoch)return;
 uiText($('test-title'),item.case.name);const definition=state.record?.stages.find(s=>s.definition.id===item.stage_id)?.definition.fixtures?.suite?.cases?.find(v=>v.id===item.case.id);renderFixtureCase(result,definition);$('test-content').prepend(button(uiMessage("Test logs"),()=>openEvidence({run_id:id,stage_id:item.stage_id,test_id:item.case.id,test_name:item.case.name})));if(state.canRetry&&!state.record?.external&&terminal.has(state.record?.status))$('test-content').prepend(button(uiMessage('Retry test'),()=>retryStage(item.stage_id,item.case.id,item.case.name)));$('test-dialog').showModal();
}

async function showTest(id,item,epoch){const c=item.case;const dialog=$('test-dialog');uiText($('test-title'),c.name);const metadata=detail(uiMessage("Test metadata"),'test-metadata',false);metadata.append(code(c));$('test-content').replaceChildren(badge(c.status));if(c.message)$('test-content').append(code(c.message));$('test-content').append(button(uiMessage("Test logs"),()=>openEvidence({run_id:id,stage_id:item.stage_id,test_id:c.id,test_name:c.name})),metadata);if(state.canRetry&&!state.record?.external&&terminal.has(state.record?.status))$('test-content').prepend(button(uiMessage('Retry test'),()=>retryStage(item.stage_id,c.id,c.name)));dialog.showModal();if(c.file){try{const source=await api('test-source?'+query({project_id:state.project,run_id:id,stage_id:item.stage_id,test_id:c.id}));if(epoch!==state.epoch||!dialog.open)return;if(source.available){$('test-content').append(el('p',(source.opens_snapshot?uiMessage("Captured source; current checkout differs."):uiMessage("Current checkout matches the executed source."))),code(source.code));if(source.vscode_uri){const link=el('a',uiMessage("Open in VS Code"));link.href=source.vscode_uri;$('test-content').append(link);}pathRow($('test-content'),uiMessage("Source path"),source.path);}else $('test-content').append(el('p',source.reason));}catch(e){if(epoch===state.epoch)$('test-content').append(el('p',e.message));}}}
$('close-test').onclick=()=>closeDialog($('test-dialog'));
function openEditor(fresh=false){state.editorKind=!fresh&&state.experiment?'experiment':'pipeline';$('editor').hidden=false;uiText(document.querySelector('#editor h2'),(state.editorKind==='experiment'?uiMessage("Experiment definition"):uiMessage("Pipeline definition")));$('pipeline-name').value=fresh?'':state.pipeline?.saved_name||'';$('definition').value=pretty(fresh?{version:1,pipeline:{stages:[{id:'build',command:{program:'',args:[],timeout_seconds:300}}]}}:state.editorKind==='experiment'?state.experimentDefinition:state.definition);buildStageEditor();$('editor-notice').replaceChildren();if(!$('editor').open)$('editor').showModal();prepareCodeEditor(fresh);}
$('new-pipeline').onclick=()=>openEditor(true);$('edit').onclick=()=>openEditor();$('close-editor').onclick=requestEditorClose;$('editor').addEventListener('cancel',event=>{event.preventDefault();requestEditorClose();});$('editor').addEventListener('close',()=>{$('editor').hidden=true;editorGeneration++;codeEditor?.destroy();codeEditor=null;});
$('new-experiment').onclick=action(async()=>{const saved=await api('catalog?'+query({kind:'saved_pipelines',project_id:state.project,limit:1}));const selected=state.entries.find(p=>p.saved_name===$('experiment-base').value);const pipeline=selected?{name:selected.saved_name,revision:selected.revision}:state.experiment?state.experimentDefinition?.pipeline:state.pipeline?.saved_name?{name:state.pipeline.saved_name,revision:state.revision}:saved.items.length?{name:saved.items[0].name,revision:saved.items[0].revision}:null;if(!pipeline){notice(uiError(uiMessage("Save a pipeline before creating an experiment.")));return;}state.editorKind='experiment';$('editor').hidden=false;uiText(document.querySelector('#editor h2'),uiMessage("New experiment"));$('pipeline-name').value='';$('definition').value=pretty({version:1,pipeline,baseline_run:null,override_definition:null,profile:null,source:null,selected_fixtures:{}});buildStageEditor();$('editor-notice').replaceChildren();if(!$('editor').open)$('editor').showModal();prepareCodeEditor();});
$('copy-definition').onclick=action(()=>copy($('definition').value));
async function validateEditor(){codeEditor?.validate();const definition=JSON.parse($('definition').value);if(state.editorKind==='experiment'){const result=await api('experiments/validate',{project_id:state.project,name:$('pipeline-name').value||'preview',expected_revision:0,definition});return result.effective_definition;}await api('pipelines/validate',definition);return definition;}
$('validate').onclick=action(async()=>{graph($('editor-graph'),await validateEditor());notice(uiMessage("Definition is valid."));});
$('save').onclick=action(async()=>{const name=$('pipeline-name').value.trim();if(!name){$('pipeline-name').focus();throw uiError(uiMessage("Enter a name."));}const definition=JSON.parse($('definition').value),kind=state.editorKind;const sameKind=(kind==='experiment')===Boolean(state.experiment);const expected_revision=sameKind&&name===state.pipeline?.saved_name?state.revision:0;await validateEditor();const saved=await api(kind==='experiment'?'experiments':'pipeline-configs',{project_id:state.project,name,expected_revision,definition});await selectProject(state.project);if(kind==='experiment')await selectExperiment({name,revision:saved.revision});else await selectPipeline({pipeline_key:'saved:'+name,name,saved_name:name,revision:saved.revision});notice(uiMessage("Saved immutable revision {0}",{"0":saved.revision}));});
$('start').onclick=action(async()=>{const project=state.project,epoch=state.epoch;const p=state.pipeline;if(!p?.saved_name)return;const body={project_id:state.project,name:p.saved_name,revision:state.revision,idempotency_key:crypto.randomUUID()};if(!state.experiment)body.profile=null;const run=await api(state.experiment?'experiment-runs':'saved-pipeline-runs',body);if(state.project!==project||state.epoch!==epoch)return;await loadHistory();await selectRun(run.id||run.run_id);});
let polling=false;
async function pollCurrentPage(){
 if(state.section==='executions'||polling||document.hidden||settingsActive||$('test-dialog').open||$('editor').open||$('schedule-dialog').open)return;
 if(document.activeElement?.matches('input,textarea,select')||document.querySelector('.row-menu[open]'))return;
 polling=true;try{
  if($('explorer').hidden&&state.run){await loadHistory();await loadRun(true);}
  else if(state.project){await refreshCatalog();}
  uiText($('sync-status'),uiMessage("Live"));uiAttribute($('sync-status'),"title",uiMessage("Last synchronized {0}",{"0":uiFormatted(new Date(),"toLocaleTimeString")}));
 }catch(e){uiText($('sync-status'),uiMessage("Reconnecting…"));uiAttribute($('sync-status'),"title",e.message);}finally{polling=false;}
}
setInterval(()=>{if(!liveConnected||Date.now()-lastCatalogRefresh>30000)pollCurrentPage();},10000);setInterval(syncLive,1000);window.addEventListener('online',pollCurrentPage);document.addEventListener('visibilitychange',()=>{syncLive();if(!document.hidden)pollCurrentPage();});
async function refreshCatalog(){
 if(state.section==='schedules'){await loadSchedules(null,true);return;}
 if(Date.now()-lastCatalogRefresh<30000)return;lastCatalogRefresh=Date.now();
 // Preserve loaded history while live updates refresh the first page only.
 if(['pipelines','experiments'].includes(state.section)&&!document.activeElement?.closest('#catalog-toolbar,#object-navigation')){
 const kind=state.section,page=catalogPages[kind];await loadCatalogPage(kind,false,page.term||'',true);
 }
}
function syncLive(){
 const key=document.hidden||!state.project||state.section==='executions'?null:state.project+':'+(state.run||'');if(key===liveKey)return;
 liveSource?.close();liveSource=null;liveKey=key;liveConnected=false;lastLiveCompletion=null;if(!key)return;
 let updating=false,dirty=false,lastSnapshot='';const project=state.project,run=state.run,source=sharedLiveSource();liveSource=source;
 source.onopen=()=>{if(liveSource===source){liveConnected=true;uiText($('sync-status'),uiMessage("Live"));}};
 source.onerror=()=>{if(liveSource===source){liveConnected=false;uiText($('sync-status'),uiMessage("Reconnecting…"));}};
 source.addEventListener('change',async event=>{if(liveSource!==source)return;if(updating){dirty=true;return;}updating=true;try{const message=JSON.parse(event.data),data=message.hint?await api('live/snapshot?'+query({project_id:project,run_id:run})):message;if(liveSource!==source)return;const signature=JSON.stringify(data);if(signature===lastSnapshot)return;lastSnapshot=signature;if(lastLiveCompletion!==null&&data.completion_cursor>lastLiveCompletion){const page=await api('completion-events?'+query({project_id:project,after:lastLiveCompletion,limit:3}));for(const item of page.events){const text=uiMessage('Run {0} · {1}',{0:item.run_id.slice(-8),1:uiStatus(item.status)});notice((['failed','timed_out','infrastructure_error'].includes(item.status)?uiError(text):text));}}lastLiveCompletion=data.completion_cursor;
 if(!document.querySelector('dialog[open]'))await pollCurrentPage();state.pollLogs?.();}catch(error){uiAttribute($('sync-status'),"title",error.message);}finally{updating=false;if(dirty&&liveSource===source){dirty=false;source.dispatchEvent(new MessageEvent('change',{data:'{"hint":true}'}));}}});
}
try{const connection=await (await fetch('/connection')).json();uiText($('server'),connection.server);state.bundled=connection.bundled===true;if(state.bundled)await authenticateBrowser();const caps=await api('capabilities');configureLive(caps.features.shared_sse_hints===true);if(caps.features.release_notifications)installReleaseNotice({anchor:$('settings-open'),api,el,button,modalClose,closeDialog,notice});state.hostArch=caps.execution?.architecture;state.canDocker=caps.features.docker_executor===true;state.apiRole=caps.access?.role||'local_owner';state.canCI=caps.features.external_ci_ingestion===true;ciNav.hidden=!state.canCI;state.canRetry=caps.features.stage_retry===true;state.canTestSearch=caps.features.test_result_filters===true;state.canDiagnostics=caps.features.diagnostic_reports===true;state.canRetention=caps.features.evidence_expiry===true&&caps.features.ranked_cache_eviction===true;state.canEvidenceFilters=caps.features.evidence_filters===true;state.canByteWindows=caps.features.evidence_byte_windows===true;state.canRunCompare=caps.features.run_comparison===true;uiText($('protocol'),uiMessage("Protocol {0}",{"0":caps.preflight_protocol.version}));if(!caps.features.project_pipeline_history||!caps.features.experiments||!caps.features.calendar_schedules||!caps.features.schedule_editing||!caps.features.evidence_retrieval||!caps.features.acceptance_criteria||!caps.features.catalog_pagination||!caps.features.client_sse||!caps.features.global_executions||!caps.features.pipeline_editor)throw uiError(uiMessage("This client requires Preflight protocol 1.43 or later. Update the server."));await loadProjects();}catch(e){notice(e);}

let scheduleTarget=null;
async function loadSchedules(before=null,refresh=false){
 const project=state.project;if(!project||((before||refresh)&&schedulesLoading))return;const request=++schedulesRequest;schedulesLoading=true;schedulesPager?.set({more:false});if(!before&&!refresh){schedulesPager?.destroy();schedulesPager=null;}try{
 const result=await api('schedules?'+query({project_id:project,before}));if(state.project!==project||request!==schedulesRequest)return;
 const scroll=$('schedules-panel').scrollTop;scheduleList?.destroy();schedulesPager?.destroy();$('schedules').replaceChildren();if(!before&&!refresh)scheduleItems=[];const updates=new Map(result.schedules.map(item=>[item.schedule_id,item]));scheduleItems=scheduleItems.map(item=>updates.get(item.schedule_id)||item);const seen=new Set(scheduleItems.map(item=>item.schedule_id));scheduleItems.push(...result.schedules.filter(item=>!seen.has(item.schedule_id)));if(!refresh)scheduleNext=result.next_cursor;
 const renderSchedule=schedule=>{
  const row=el('div',undefined,'card');row.dataset.scheduleId=schedule.schedule_id;
  const heading=el('div',undefined,'heading');heading.append(el('strong',schedule.name),badge(schedule.state));row.append(heading,el('p',((schedule.experiment?uiMessage("Experiment · {0} · revision {1}",{"0":schedule.experiment.name,"1":schedule.experiment.revision}):uiMessage("{0} · revision {1}",{"0":schedule.pipeline.name,"1":schedule.pipeline.revision}))),'muted'),el('p',uiMessage('{0} · {1}',{0:uiFormatted(new Date(schedule.at_ms),'toLocaleString'),1:Intl.DateTimeFormat().resolvedOptions().timeZone})),copyButton(schedule,uiMessage("Copy schedule")));
  if(schedule.repeat)row.append(el('p',uiMessage("{0} · overlap: {1}",{"0":schedule.repeat.cron?schedule.repeat.cron+' · '+schedule.repeat.timezone:uiMessage('Every {0} min',{0:schedule.repeat.every_seconds/60}),"1":schedule.repeat.overlap}),'muted'));
  row.append(button(uiMessage("History"),()=>showScheduleHistory(project,schedule)));
  const change=async action=>{await api('schedules/change',{project_id:project,schedule_id:schedule.schedule_id,action,expected_revision:schedule.revision,request_id:crypto.randomUUID(),actor:'user',reason:'Changed from optional client'});await loadSchedules();};
  if(['pending','paused'].includes(schedule.state))row.append(button(uiMessage("Edit"),()=>openSchedule(schedule)));
  if(schedule.state==='pending')row.append(button(uiMessage("Pause"),()=>change('pause')));
  if(schedule.state==='paused')row.append(button(uiMessage("Resume"),()=>change('resume')));
  if(['pending','paused'].includes(schedule.state))row.append(button(uiMessage("Cancel schedule"),()=>change('cancel')));
  if(schedule.state!=='dispatching')row.append(button(uiMessage("Delete schedule"),()=>change('delete')));
  if(schedule.run_id)row.append(button(uiMessage("Open run"),async()=>{if(state.project!==project)return;if(schedule.experiment)await selectExperiment({name:schedule.experiment.name});else await selectPipeline({pipeline_key:'saved:'+schedule.pipeline.name,name:schedule.pipeline.name,saved_name:schedule.pipeline.name});if(state.project===project)await selectRun(schedule.run_id);}));
  return row;
 };
 if(scheduleItems.length){scheduleList=mountVirtualCards($('schedules-panel'),{host:$('schedules'),render:renderSchedule,key:item=>item.schedule_id,minWidth:300,gap:16,estimate:270});scheduleList.setItems(scheduleItems);}
 if(!scheduleItems.length&&!before){const empty=el('div',(state.entries.some(p=>p.saved_name)?uiMessage("No schedules yet."):uiMessage("Save a pipeline to schedule runs.")),'empty-state');if(!state.entries.some(p=>p.saved_name))empty.append(button(uiMessage("Create pipeline"),()=>openEditor(true)));$('schedules').append(empty);}
 schedulesPager?.destroy();schedulesPager=mountInfiniteScroll($('schedules-panel'),()=>loadSchedules(scheduleNext),{host:$('schedules')});schedulesPager.set({more:scheduleNext});$('schedules-panel').scrollTop=scroll;
 }catch(error){if(state.project===project&&request===schedulesRequest){schedulesPager?.destroy();schedulesPager=mountInfiniteScroll($('schedules-panel'),()=>loadSchedules(before),{host:$('schedules')});schedulesPager.set({more:true,error:true});}throw error;}finally{if(request===schedulesRequest)schedulesLoading=false;}
}
async function showScheduleHistory(project,schedule){
 const dialog=el('dialog');uiAttribute(dialog,'aria-label',uiMessage("Schedule history"));const list=el('div');dialog.append(el('h2',schedule.name),modalClose(button(uiMessage("Close"),()=>closeDialog(dialog))),list);let cursor=null;
 const more=button(uiMessage("Older occurrences"),async()=>{const result=await api('schedule-occurrences?'+query({project_id:project,schedule_id:schedule.schedule_id,before_at:cursor}));if(!dialog.open)return;for(const occurrence of result.occurrences){const item=el('div',undefined,'card');item.append(el('strong',uiFormatted(new Date(occurrence.at_ms),"toLocaleString")),badge(occurrence.run_status||occurrence.state),el('p',occurrence.reason));if(occurrence.run_id)item.append(button(uiMessage("Open run"),async()=>{closeDialog(dialog);if(state.project!==project)return;if(schedule.experiment)await selectExperiment({name:schedule.experiment.name});else await selectPipeline({pipeline_key:'saved:'+schedule.pipeline.name,name:schedule.pipeline.name,saved_name:schedule.pipeline.name});await selectRun(occurrence.run_id);}));list.append(item);}cursor=result.next_cursor;more.hidden=cursor===null;if(!list.childElementCount)list.append(el('p',uiMessage("No occurrences yet.")));});dialog.append(more);dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();await more.onclick();
}
async function openSchedule(schedule=null){
 const project=state.project,saved=(await api('catalog?'+query({kind:schedule?.experiment?'experiments':'saved_pipelines',project_id:state.project,limit:25}))).items;
 const profiles=await api('projects/'+encodeURIComponent(project)+'/toolchains');if(state.project!==project)return;
 scheduleTarget={project_id:project,schedule_id:schedule?.schedule_id,expected_revision:schedule?.revision,grace_ms:schedule?.grace_ms??60000};
 const pipeline=$('schedule-pipeline'),revision=$('schedule-revision'),profile=$('schedule-profile'),profileRevision=$('schedule-profile-revision');
 const targetKind=$('schedule-kind');targetKind.value=schedule?.experiment?'experiment':'pipeline';
 const chosen=schedule?.experiment||schedule?.pipeline;
 pipeline.replaceChildren(...saved.map(p=>uiOption(p.name,p.saved_name||p.name)));if(chosen&&!saved.some(p=>p.name===chosen.name))pipeline.append(uiOption(chosen.name,chosen.name));pipeline.value=chosen?.name||state.pipeline?.saved_name||saved[0]?.name||'';
 const setRevision=current=>revisionOptions(revision,current,current);setRevision(chosen?.revision||saved.find(p=>p.name===pipeline.value)?.revision||1);
 const choiceChange=async()=>{const cfg=await api((targetKind.value==='experiment'?'experiment?':'pipeline-configs?')+query({project_id:project,name:pipeline.value}));setRevision(cfg.revision);};pipeline.onchange=action(choiceChange);
 const configureTarget=async(reset=false)=>{const experiment=targetKind.value==='experiment';profile.disabled=experiment;if(experiment){profile.value='';profileRevision.disabled=true;}
 if(reset){const page=await api('catalog?'+query({kind:experiment?'experiments':'saved_pipelines',project_id:project,limit:25}));pipeline.replaceChildren(...page.items.map(p=>uiOption(p.name,p.saved_name||p.name)));if(pipeline.value)await choiceChange();}
 remoteChoices(pipeline,experiment?'experiments':'saved_pipelines');};targetKind.onchange=action(()=>configureTarget(true));
 profile.replaceChildren(uiOption(uiMessage("No profile"),''),...profiles.toolchains.map(p=>uiOption(p.name,p.name)));if(schedule?.profile&&!profiles.toolchains.some(p=>p.name===schedule.profile.name))profile.append(uiOption(schedule.profile.name,schedule.profile.name));profile.value=schedule?.profile?.name||'';
 const setProfileRevision=current=>{profileRevision.disabled=!profile.value;if(profile.value)revisionOptions(profileRevision,profiles.toolchains.find(p=>p.name===profile.value)?.revision||current,current);else profileRevision.replaceChildren(uiOption('—',''));};setProfileRevision(schedule?.profile?.revision||1);profile.onchange=()=>setProfileRevision(profiles.toolchains.find(p=>p.name===profile.value)?.revision||1);
 await configureTarget();uiText($('schedule-title'),(schedule?uiMessage("Edit schedule"):uiMessage("New schedule")));uiText($('schedule-submit'),(schedule?uiMessage("Save changes"):uiMessage("Create schedule")));uiText($('schedule-target'),'');
 $('schedule-name').value=schedule?.name||'';$('schedule-time').value=new Date(schedule?.at_ms||Date.now()+3600000).toISOString().slice(0,16);scheduleTarget.originalTime=$('schedule-time').value;scheduleTarget.originalAt=schedule?.at_ms;
 const repeat=schedule?.repeat; $('schedule-repeat').value=repeat?.cron?'calendar':repeat?'interval':'once';$('schedule-interval').value=repeat?.every_seconds?repeat.every_seconds/60:60;$('schedule-overlap').value=repeat?.overlap||'skip';$('schedule-misfire').value=schedule?.misfire||'run_once';$('schedule-cron').value=repeat?.cron||'0 9 * * 1-5';
 const zones=['UTC',...(Intl.supportedValuesOf?.('timeZone')||[])],zone=repeat?.timezone||Intl.DateTimeFormat().resolvedOptions().timeZone;if(!zones.includes(zone))zones.unshift(zone);$('schedule-timezone').replaceChildren(...zones.map(z=>uiOption(z,z)));$('schedule-timezone').value=zone;
 $('schedule-repeat').onchange();$('schedule-dialog').showModal();syncSelects();
}
$('new-schedule').onclick=action(()=>openSchedule());
$('schedule-repeat').onchange=()=>{$('schedule-interval-fields').hidden=$('schedule-repeat').value==='once';$('schedule-interval').parentElement.querySelector('label[for=schedule-interval]').hidden=$('schedule-repeat').value!=='interval';$('schedule-interval').hidden=$('schedule-repeat').value!=='interval';$('schedule-calendar-fields').hidden=$('schedule-repeat').value!=='calendar';$('schedule-preview-times').replaceChildren();};
function scheduledTime(){const value=$('schedule-time').value;return scheduleTarget?.originalTime===value&&scheduleTarget.originalAt!==undefined?scheduleTarget.originalAt:Date.parse(value+'Z');}
function scheduleRepeat(){const mode=$('schedule-repeat').value,overlap=$('schedule-overlap').value;return mode==='interval'?{every_seconds:Number($('schedule-interval').value)*60,overlap}:mode==='calendar'?{cron:$('schedule-cron').value,timezone:$('schedule-timezone').value,overlap}:null;}
$('schedule-preview').onclick=action(async()=>{const result=await api('schedules/preview',{at_ms:scheduledTime(),repeat:scheduleRepeat(),count:5});$('schedule-preview-times').replaceChildren(...result.at_ms.map(at=>el('li',uiMessage("{0} {1}",{"0":uiFormatted(new Date(at),"toLocaleString",{timeZone:scheduleRepeat()?.timezone||'UTC',dateStyle:'medium',timeStyle:'short'}),"1":(scheduleRepeat()?.timezone || uiMessage("UTC"))}))));});
$('close-schedule').onclick=()=>closeDialog($('schedule-dialog'));
$('schedule-form').onsubmit=action(async event=>{event.preventDefault();
 if(!scheduleTarget||scheduleTarget.project_id!==state.project)throw uiError(uiMessage("Project changed; reopen the scheduler."));
 const at_ms=scheduledTime();if(!Number.isFinite(at_ms))throw uiError(uiMessage("Choose a UTC date and time."));
 const definition={project_id:state.project,name:$('schedule-name').value.trim(),pipeline:{name:$('schedule-pipeline').value,revision:Number($('schedule-revision').value)},profile:$('schedule-profile').value?{name:$('schedule-profile').value,revision:Number($('schedule-profile-revision').value)}:null,at_ms,repeat:scheduleRepeat(),misfire:$('schedule-misfire').value,grace_ms:scheduleTarget.grace_ms,request_id:crypto.randomUUID(),actor:'user',reason:scheduleTarget.schedule_id?'Edited from optional client':'Scheduled from optional client'};
 if($('schedule-kind').value==='experiment'){definition.experiment={name:definition.pipeline.name,revision:definition.pipeline.revision};const selected=await api('experiment?'+query({project_id:state.project,...definition.experiment}));definition.pipeline=selected.definition.pipeline;definition.profile=null;}
 await api(scheduleTarget.schedule_id?'schedules/update':'schedules',scheduleTarget.schedule_id?{schedule_id:scheduleTarget.schedule_id,expected_revision:scheduleTarget.expected_revision,definition}:definition);
 closeDialog($('schedule-dialog'));await loadSchedules();notice((scheduleTarget.schedule_id?uiMessage("Schedule updated."):uiMessage("Schedule created.")));
 });

for(const id of ['history-panel']){
 const panel=$(id),key='preflight:panel:'+id;
 try{if(localStorage.getItem(key)!==null)panel.open=localStorage.getItem(key)==='true';}catch{}
 panel.addEventListener('toggle',()=>{try{localStorage.setItem(key,String(panel.open));}catch{}});
}
function toggleHistory(value){$('run-workspace').classList.toggle('history-collapsed',value);pipelineActionLabel('history-toggle',value?uiMessage('Show run history'):uiMessage('Hide run history'));$('history-toggle').setAttribute('aria-expanded',String(!value));$('history-toggle').setAttribute('aria-pressed',String(!value));try{localStorage.setItem('preflight:history-hidden-v2',String(value));}catch{}}
$('history-toggle').onclick=()=>toggleHistory(!$('run-workspace').classList.contains('history-collapsed'));
try{toggleHistory(localStorage.getItem('preflight:history-hidden-v2')!=='false');}catch{}

function buildStageEditor(){
 const target=$('stage-editor'),focusName=target.contains(document.activeElement)&&document.activeElement?.getAttribute('role')==='combobox'?document.activeElement.getAttribute('aria-label'):null;target.replaceChildren();$('advanced-editor').open=false;
 if(state.editorKind==='experiment'){const content=el('div');target.append(content);buildExperimentEditor(content).then(()=>{syncSelects();if(focusName&&content.isConnected)for(const choice of content.querySelectorAll('[role=combobox]'))if(choice.getAttribute('aria-label')===focusName)choice.focus({preventScroll:true});}).catch(error=>{if(content.isConnected){content.replaceChildren(el('p',error.message,'failed'),button(uiMessage("Retry"),buildStageEditor));}});return;}
 let definition;try{definition=JSON.parse($('definition').value);}catch{return;}
 renderStageFields(target,definition,()=>{$('definition').value=pretty(definition);codeEditor?.setText($('definition').value);updateEditorStatus();},buildStageEditor);
}
function renderStageFields(target,definition,update,rebuild){
 const stages=definition.pipeline?.stages;if(!stages)return;
 const field=(parent,label,value,write,multiline=false)=>{const wrapper=el('label',label),input=el(multiline?'textarea':'input');input.value=value;input.oninput=()=>{write(input.value);update();};if(multiline)input.rows=3;wrapper.append(input);parent.append(wrapper);return input;};
 
 for(const stage of stages){const card=el('details',undefined,'stage-form');card.open=stage===stages[0]||state.editStage===stage.id;card.append(el('summary',stage.id));const fields=el('div',undefined,'stage-fields');renderTestConfiguration(fields,stage,update,rebuild);
 if(stage.command){field(fields,uiMessage("Program"),stage.command.program,v=>stage.command.program=v);field(fields,uiMessage("Working directory"),stage.cwd||'.',v=>stage.cwd=v);if((stage.command.args||[]).some(arg=>arg.includes('\n'))){fields.append(el('p',uiMessage("Arguments contain line breaks; edit them in Advanced JSON to preserve exact values."),'muted'));}else{field(fields,uiMessage("Arguments · one per line"),(stage.command.args||[]).join('\n'),v=>stage.command.args=v===''?[]:v.split('\n'),true);}const timeout=field(fields,uiMessage("Timeout (seconds)"),stage.command.timeout_seconds||300,v=>stage.command.timeout_seconds=Number(v));timeout.type='number';timeout.min='1';}
 else fields.append(el('p',uiMessage("This stage uses fixtures. Edit its configuration in Advanced JSON."),'muted'));
 if(!stage.fixtures){renderEnvironment({target:fields,stage,stages,field,el,button,update,rebuild,api,projectId:state.project,arch:state.hostArch});renderStageOutputs(fields,stage,update,rebuild);}
 const dependencies=el('fieldset');dependencies.append(el('legend',uiMessage("Run after")));for(const other of stages){if(other.id===stage.id)continue;const label=el('label'),check=el('input');check.type='checkbox';check.checked=(stage.needs||[]).includes(other.id);check.onchange=()=>{stage.needs=check.checked?[...new Set([...(stage.needs||[]),other.id])]:(stage.needs||[]).filter(id=>id!==other.id);update();};label.append(check,uiNode(other.id));dependencies.append(label);}if(stages.length===1)dependencies.append(el('span',uiMessage("No other stages."),'muted'));fields.append(dependencies);
 const remove=button(uiMessage("Remove stage"),()=>{if(stages.some(s=>(s.needs||[]).includes(stage.id))){notice(uiError(uiMessage("Remove dependencies on {0} first.",{"0":stage.id})));return;}definition.pipeline.stages=stages.filter(s=>s!==stage);update();rebuild();});remove.className='danger';fields.append(remove);card.append(fields);target.append(card);}
 target.append(button(uiMessage("Add stage"),()=>{let index=stages.length+1;while(stages.some(s=>s.id==='stage_'+index))index++;state.editStage='stage_'+index;stages.push({id:'stage_'+index,command:{program:'',args:[],timeout_seconds:300}});update();rebuild();}));
 renderAcceptanceEditor(target,definition,update,rebuild);
}
$('definition').addEventListener('change',()=>{const expanded=$('advanced-editor').open;buildStageEditor();$('advanced-editor').open=expanded;});

function showRunWorkspace(section){executions.hide();$('breadcrumb').hidden=false;$('execution-nav').classList.remove('active');
 hideSettings();
 window.scrollTo(0,0);
 state.section=section;$('explorer').hidden=true;$('run-workspace').hidden=false;document.querySelector('.page-heading').hidden=false;
 updateSectionNavigation();
}
function updateSectionNavigation(){
 document.querySelectorAll('#sections button').forEach(b=>{const active=b.dataset.section===state.section;b.classList.toggle('active',active);b.setAttribute('aria-current',active?'page':'false');});
 uiText($('back-section'),uiMessage(state.section==='ci'?'External CI':state.section[0].toUpperCase()+state.section.slice(1)));uiText($('breadcrumb-current'),($('explorer').hidden?' / '+(state.pipeline?.name||''):''));
 $('object-navigation').hidden=['schedules','evidence','executions','ci'].includes(state.section);$('pipelines').hidden=state.section!=='pipelines';$('experiments').hidden=state.section!=='experiments';
 uiText($('list-title'),(state.section==='experiments'?uiMessage("Experiments"):uiMessage("Pipelines")));uiAttribute($('pipeline-filter'),"placeholder",uiMessage("Filter {0}…",{"0":state.section}));
 uiAttribute($('sidebar-create'),'aria-label',(state.section==='experiments'?uiMessage("Create experiment"):uiMessage("Create pipeline")));
 uiText($('pipeline-count'),state.entries.length+(catalogPages.pipelines.next?'+':''));uiText($('experiment-count'),state.experiments.length+(catalogPages.experiments.next?'+':''));
}
function showSection(section){if(section!=='evidence'){state.disposeEvidence?.();state.disposeEvidence=null;}externalCI.hide();executions.hide();$('breadcrumb').hidden=false;$('execution-nav').classList.remove('active');
 closeMobileMenu();hideSettings();
 window.scrollTo(0,0);
 state.epoch++;state.section=section;if(['pipelines','experiments'].includes(section)){$('pipeline-filter').value=catalogPages[section].term||'';$('catalog-search').value=$('pipeline-filter').value;}$('explorer').hidden=false;$('run-workspace').hidden=true;document.querySelector('.page-heading').hidden=true;$('editor').hidden=true;notice('');
 $('new-pipeline').hidden=section!=='pipelines';$('new-experiment').hidden=section!=='experiments';$('new-schedule').hidden=section!=='schedules';$('experiment-base').hidden=true;
 const saved=state.entries.filter(p=>p.saved_name);$('experiment-base').replaceChildren(...saved.map(p=>uiOption(p.name,p.saved_name)));if(state.pipeline?.saved_name)$('experiment-base').value=state.pipeline.saved_name;
 if(!$('experiment-base').value&&saved.length)$('experiment-base').value=saved[0].saved_name;
 $('new-experiment').disabled=false;$('new-schedule').disabled=false;
 $('schedules-panel').hidden=section!=='schedules';$('schedules-panel').open=true;
 uiText($('explorer-title'),{pipelines:uiMessage("Pipelines"),experiments:uiMessage("Experiments"),schedules:uiMessage("Schedules"),evidence:uiMessage("Evidence"),ci:"External CI"}[section]);
 uiText($('explorer-subtitle'),{pipelines:uiMessage("Builds, stages and test results."),experiments:uiMessage("Compare changes in separate runs."),schedules:uiMessage("Schedule a saved pipeline."),evidence:uiMessage("Build logs, test output and cited context."),ci:"Completed provider runs, logs, tests and artifacts."}[section]);uiAttribute($('experiment-base'),'aria-label',(section==='schedules'?uiMessage("Pipeline to schedule"):uiMessage("Base pipeline")));$('experiment-base').disabled=!saved.length; if(!saved.length)$('experiment-base').append(uiOption(uiMessage("No saved pipelines"),''));uiAttribute($('new-experiment'),"title",uiAttribute($('new-schedule'),"title",''));
 updateSectionNavigation();renderCatalog();filterNavigation();$('evidence-panel').hidden=section!=='evidence';if(section==='evidence')renderEvidence().catch(notice);if(section==='ci')externalCI.show(state.project).catch(notice);writeRoute();
}
function renderCatalog(){
 const target=$('catalog'),savedScroll=target.scrollTop;catalogPager?.destroy();catalogList?.destroy();catalogList=null;target.replaceChildren();if(['evidence','ci'].includes(state.section)){target.hidden=true;$('catalog-toolbar').hidden=true;return;}target.hidden=state.section==='schedules';$('catalog-toolbar').hidden=state.section==='schedules';
 const page=catalogPages[state.section]||{},items=state.section==='experiments'?state.experiments:state.entries;let layout=narrowNavigation.matches?'cards':'list';try{layout=narrowNavigation.matches?'cards':localStorage.getItem('preflight:object-layout')||'list';}catch{}
 target.classList.add('virtual-catalog');target.classList.toggle('list-layout',layout==='list');$('layout-list').setAttribute('aria-pressed',String(layout==='list'));$('layout-cards').setAttribute('aria-pressed',String(layout!=='list'));
 const table=el('table',undefined,'object-table');if(layout==='list'){const header=el('tr');for(const title of [uiMessage("Name"),uiMessage("Configuration"),uiMessage("Runs"),uiMessage("Actions")])header.append(el('th',title));table.append(header);target.append(table);}
 const renderItem=item=>{const experiment=state.section==='experiments',open=()=>experiment?selectExperiment(item):selectPipeline(item),description=(experiment?uiMessage("Isolated experiment"):(item.saved_name?uiMessage("Saved · revision {0}",{"0":item.revision}):uiMessage("Captured definition")));
 const displayName=experiment?item.name:pipelineListName(item),card=el(layout==='list'?'tr':'article',undefined,'catalog-card'),title=button(displayName,open);card.dataset.key=item.pipeline_key||item.key||'';card.dataset.name=item.name.toLowerCase();title.className='catalog-title';uiAttribute(title,"title",displayName);
 const actions=el('div',undefined,'catalog-actions');actions.append(button(uiMessage("Open"),open));
 const menu=el('details',undefined,'row-menu');const menuTitle=el('summary','•••');uiAttribute(menuTitle,'aria-label',uiMessage("Actions for {0}",{"0":displayName}));menu.append(menuTitle);const choices=el('div');choices.popover='auto';uiAttribute(choices,'aria-label',uiMessage("Pipeline actions"));menu.addEventListener('toggle',()=>{if(!menu.isConnected)return;if(!menu.open){choices.hidePopover();return;}choices.showPopover();const anchor=menuTitle.getBoundingClientRect(),box=choices.getBoundingClientRect();choices.style.left=Math.max(8,Math.min(anchor.left,innerWidth-box.width-8))+'px';choices.style.top=Math.max(8,anchor.bottom+box.height+6<=innerHeight?anchor.bottom+6:anchor.top-box.height-6)+'px';});choices.addEventListener('toggle',event=>{if(event.newState==='closed')menu.open=false;});if(!experiment)choices.append(button(uiMessage("Manage pipeline"),()=>{menu.open=false;return management.open({...managementContext(),pipeline_key:item.pipeline_key,pipeline_name:item.name});}));choices.append(button(uiMessage("Edit definition"),async()=>{await open();openEditor();}),copyButton(displayName,uiMessage("Copy name")));menu.append(choices);actions.append(menu);
 if(layout==='list'){const name=el('td'),descriptionCell=el('td',description,'muted'),count=el('td',(item.runs===undefined?'—':String(item.runs)),'muted'),actionCell=el('td');name.append(title);actionCell.append(actions);card.append(name,descriptionCell,count,actionCell);}else{card.append(title,el('p',description+(item.runs===undefined?'':' · '+item.runs+' runs'),'muted'),actions);}return card;};
 if(items.length){catalogList=layout==='list'?mountVirtualList(target,{host:table,tag:'tbody',render:renderItem,key:item=>item.key,estimate:64}):mountVirtualCards(target,{render:renderItem,key:item=>item.key});catalogList.setItems(items);}
 if(!items.length){table.remove();if(!page.loading)target.append(el('div',(page.term?uiMessage("No matches."):(state.section==='experiments'?uiMessage("No experiments yet."):uiMessage("No pipelines yet. Create your first pipeline."))),'empty-state'));}
 if(page.loading)target.append(loadingIndicator('Loading…'));else {const kind=state.section;catalogPager=mountInfiniteScroll(target,()=>loadCatalogPage(kind,true));catalogPager.set({more:page.next,error:page.error});}
 if(items.length){const empty=el('div',uiMessage("No objects match your search."),'empty-state');empty.id='catalog-empty';empty.hidden=true;target.append(empty);}target.scrollTop=savedScroll;
}
function filterNavigation(){const q=$('pipeline-filter').value.trim().toLowerCase();$('catalog-search').value=$('pipeline-filter').value;document.querySelectorAll('#pipelines button[data-key],#experiments button[data-key]').forEach(b=>b.hidden=!`${b.textContent} ${b.dataset.key}`.toLowerCase().includes(q));let found=0;document.querySelectorAll('.catalog-card').forEach(c=>{c.hidden=!`${c.dataset.name} ${c.dataset.key||''}`.toLowerCase().includes(q);if(!c.hidden)found++;});if($('catalog-empty'))$('catalog-empty').hidden=found>0||!catalogPages[state.section]?.loaded||!!catalogPages[state.section]?.loading||!!catalogPages[state.section]?.error;}
$('catalog-search').oninput=()=>{$('pipeline-filter').value=$('catalog-search').value;filterNavigation();searchCatalog();};
for(const [id,layout]of [['layout-list','list'],['layout-cards','cards']])$(id).onclick=()=>{try{localStorage.setItem('preflight:object-layout',layout);}catch{}renderCatalog();filterNavigation();};
$('sections').onclick=event=>{const item=event.target.closest('button[data-section]');if(item)showSection(item.dataset.section);};
$('sidebar-create').onclick=()=>{if(state.section==='experiments')$('new-experiment').click();else openEditor(true);};

function formatBytes(bytes){if(!Number.isFinite(bytes))return uiMessage('Size unavailable');if(bytes<1024)return bytes+' B';const units=['KiB','MiB','GiB'];let n=bytes/1024,i=0;while(n>=1024&&i<units.length-1){n/=1024;i++;}return n.toFixed(1)+' '+units[i];}
function pathRow(target,label,value){const row=el('div',undefined,'path-row');row.append(el('span',label,'muted'),value===null||value===undefined?el('code','—'):copyField(el('code',String(value)),String(value),uiMessage("Copy {0}",{"0":label})));target.append(row);}
function facts(value,depth=0){const box=el('div',undefined,'facts');if(value===null||typeof value!=='object'){box.append(el('span',(value===null?uiMessage("Not recorded"):String(value))));return box;}const entries=Object.entries(value).filter(([,v])=>v!==undefined&&v!==null&&v!=='');if(!entries.length){box.append(el('span',(Array.isArray(value)?uiMessage("None"):uiMessage("Not recorded")),'muted'));return box;}for(const [key,v]of entries){const label=key.replaceAll('_',' ');if(v&&typeof v==='object'){const group=el('details');group.open=depth<1;group.append(el('summary',label+(Array.isArray(v)?' · '+v.length:'')),facts(v,depth+1));box.append(group);}else pathRow(box,label,v);}return box;}
$('theme-switch').onclick=event=>{const choice=event.target.closest('button[data-theme]');if(choice){$('theme').value=choice.dataset.theme;theme();}};
$('back-section').onclick=()=>showSection(state.section);
$('copy-link').onclick=action(()=>copy(location.href));

function writeRoute(replace=false){
 if(state.restoring)return;
 if(state.section==='executions'){const path=routePath({section:'executions',...executions.filters()});if(path!==location.pathname+location.search){history.pushState(null,'',path);animatePage(document.querySelector('main'),path);}return;}
 if(settingsActive)return;
 if(!state.project)return;
 const values={project:state.project,section:state.section};
 if($('explorer').hidden&&state.pipeline){values.pipeline=state.pipeline.pipeline_key;if(state.run)values.run=state.run;values.tab=state.tab;if(state.stage)values.stage=state.stage;if(state.tab==='Tests'&&state.testFilters?.search)values.test=state.testFilters.search;}
 const path=routePath(values);if(path!==location.pathname+location.search+location.hash){history[replace?'replaceState':'pushState'](null,'',path);animatePage(document.querySelector('main'),path);}
 uiText($('breadcrumb-current'),($('explorer').hidden?' / '+(state.pipeline?.name||''):''));
}
async function restoreRoute(route){
 state.restoring=true;let valid=false;
 try{
  if(route.has('error'))throw uiError(route.get('error'));
  if(route.get('section')==='executions'){showExecutions(Object.fromEntries(['view','kind','project_id','query'].filter(k=>route.has(k)).map(k=>[k,route.get(k)])));valid=true;return;}
  if(route.has('settings')){await showSettings();valid=true;return;}
  if(route.get('project')&&![...$('projects').options].some(p=>p.value===route.get('project'))){const project=await api('projects/'+encodeURIComponent(route.get('project')));$('projects').append(uiOption(project.name,project.id));syncSelects();}
  const project=route.get('project');if(project&&[...$('projects').options].some(p=>p.value===project)&&project!==state.project){$('projects').value=project;await selectProject(project);}
  const section=['pipelines','experiments','schedules','evidence','ci'].includes(route.get('section'))?route.get('section'):'pipelines';
  const key=route.get('pipeline');let item=section==='experiments'?state.experiments.find(p=>'experiment:'+p.name===key):state.entries.find(p=>p.pipeline_key===key);
  if(key&&!item){if(key.startsWith('saved:')){const name=key.slice(6),cfg=await api('pipeline-configs?'+query({project_id:state.project,name}));item={pipeline_key:key,name,saved_name:name,revision:cfg.revision};state.entries.push(item);}else if(key.startsWith('experiment:')){const name=key.slice(11),cfg=await api('experiment?'+query({project_id:state.project,name}));item={name,revision:cfg.revision};state.experiments.push(item);}else if(route.get('run')){const run=await api('runs/'+encodeURIComponent(route.get('run')));if(run.project_id!==state.project)throw uiError(uiMessage("Run project mismatch"));item={pipeline_key:key,name:'Pipeline '+key.slice(-12)};state.entries.push(item);}else throw uiError(uiMessage("Pipeline not found"));renderNavigation();}
  if(item&&section!=='schedules'){if(section==='experiments')await selectExperiment(item);else await selectPipeline(item);if(route.get('run')&&state.run!==route.get('run'))await selectRun(route.get('run'));if(tabs.includes(route.get('tab'))){state.stage=route.get('stage');state.tab=route.get('tab');if(route.has('test'))state.testFilters={search:route.get('test'),status:'all',kind:''};renderTabs();await renderView();}}
  else showSection(section);
  valid=true;
 }catch(e){clearPipeline();$('explorer').hidden=false;$('run-workspace').hidden=true;document.querySelector('.page-heading').hidden=true;uiText($('explorer-title'),uiMessage("Page unavailable"));uiText($('explorer-subtitle'),e.message);$('catalog').replaceChildren(button(uiMessage("Open project pipelines"),()=>showSection('pipelines')));$('catalog').hidden=false;$('schedules-panel').hidden=true;notice(e);}finally{state.restoring=false;if(valid){writeRoute(true);animatePage(document.querySelector('main'),location.pathname+location.search);}}
}
window.addEventListener('popstate',()=>restoreRoute(readRoute(new URL(location.href))));
await restoreRoute(initialRoute);$('pipeline-filter').disabled=$('catalog-search').disabled=!state.project;

function revisionOptions(select,latest,current){
 const revisions=new Set([current,...Array.from({length:Math.min(latest,100)},(_,i)=>latest-i)].filter(n=>n>0));
 select.replaceChildren(...[...revisions].sort((a,b)=>b-a).map(n=>uiOption(uiMessage("Revision {0}",{"0":n}),String(n))));select.value=String(current);
}
function runLabel(run){const name=run.experiment_name||run.pipeline_name;return uiMessage('{0}{1} · {2} · {3}',{0:name?name+' · ':'',1:timestamp(run.created_at),2:uiStatus(run.status),3:run.run_id.slice(-8)});}
async function buildExperimentEditor(target){
 let definition;try{definition=JSON.parse($('definition').value);}catch{return;}
 target.className='experiment-fields';
 const project=state.project,update=()=>{$('definition').value=pretty(definition);};
 const field=(label,value,write,rows=0)=>{const wrapper=el('label',label),input=el(rows?'textarea':'input');uiAttribute(input,'aria-label',label);input.value=value??'';if(rows)input.rows=rows;input.oninput=()=>{write(input.value);update();};wrapper.append(input);target.append(wrapper);return input;};
 const choice=(label)=>{const wrapper=el('label',label),input=el('select');uiAttribute(input,'aria-label',label);wrapper.append(input);target.append(wrapper);return input;};
 target.append(el('p',uiMessage("Loading choices…"),'muted'));
 const [saved,profiles,history]=await Promise.all([
  api('pipeline-configs?'+query({project_id:project,name:definition.pipeline.name,revision:definition.pipeline.revision})),
  api('projects/'+encodeURIComponent(project)+'/toolchains'),
  api('run-history?'+query({project_id:project,include_experiments:true,limit:100}))
 ]);
 if(!target.isConnected||!$('editor').open||state.project!==project)return;target.replaceChildren();
 const base=choice(uiMessage("Base pipeline"));for(const p of state.entries.filter(p=>p.saved_name))base.append(uiOption(p.name,p.saved_name));if(![...base.options].some(p=>p.value===definition.pipeline.name))base.append(uiOption(definition.pipeline.name,definition.pipeline.name));base.value=definition.pipeline.name;remoteChoices(base,'saved_pipelines');
 const revision=choice(uiMessage("Pipeline revision"));revisionOptions(revision,state.entries.find(p=>p.saved_name===base.value)?.revision||definition.pipeline.revision,definition.pipeline.revision);
 base.onchange=async()=>{const selected=await api('pipeline-configs?'+query({project_id:project,name:base.value}));definition.pipeline={name:base.value,revision:selected.revision};definition.baseline_run=null;definition.selected_fixtures={};definition.override_definition=null;update();buildStageEditor();};
 revision.onchange=()=>{definition.pipeline.revision=Number(revision.value);definition.selected_fixtures={};update();buildStageEditor();};
 const baseline=choice(uiMessage("Compare with run"));baseline.append(uiOption(uiMessage("No baseline"),''));let before=history.next_cursor;
 const appendRuns=rows=>{for(const run of rows.filter(r=>terminal.has(r.status)))if(![...baseline.options].some(o=>o.value===run.run_id))baseline.append(uiOption(runLabel(run),run.run_id));};appendRuns(history.runs);
 if(definition.baseline_run&&![...baseline.options].some(o=>o.value===definition.baseline_run)){
  const run=await api('runs/'+encodeURIComponent(definition.baseline_run));if(!target.isConnected)return;baseline.append(uiOption(runLabel(run),run.run_id));
 }
 baseline.value=definition.baseline_run||'';baseline.onchange=()=>{definition.baseline_run=baseline.value||null;update();};
 if(before){const more=button(uiMessage("Older runs"),async()=>{const page=await api('run-history?'+query({project_id:project,limit:100,before}));if(!target.isConnected)return;appendRuns(page.runs);before=page.next_cursor;more.hidden=!before;});target.append(more);bindPageChoices(more,[baseline]);}
 const profile=choice(uiMessage("Toolchain profile"));profile.append(uiOption(uiMessage("No profile"),''));for(const item of profiles.toolchains)profile.append(uiOption(item.name,item.name));
 if(definition.profile&&![...profile.options].some(o=>o.value===definition.profile.name))profile.append(uiOption(definition.profile.name,definition.profile.name));profile.value=definition.profile?.name||'';
 const profileRevision=choice(uiMessage("Profile revision"));if(definition.profile)revisionOptions(profileRevision,profiles.toolchains.find(p=>p.name===profile.value)?.revision||definition.profile.revision,definition.profile.revision);profileRevision.disabled=!definition.profile;if(!definition.profile)profileRevision.append(uiOption('—',''));
 profile.onchange=()=>{const selected=profiles.toolchains.find(p=>p.name===profile.value);definition.profile=selected?{name:selected.name,revision:selected.revision}:null;update();buildStageEditor();};
 profileRevision.onchange=()=>{if(definition.profile){definition.profile.revision=Number(profileRevision.value);update();}};
 const modeLabel=el('label',uiMessage("Source snapshot")),mode=el('select');uiAttribute(mode,'aria-label',uiMessage("Source snapshot"));for(const [value,label]of [['current',uiMessage("Current project snapshot")],['git',uiMessage("Exact Git commit and patches")],['dirty',uiMessage("Git working tree")],['multi',uiMessage("Multiple repositories (Advanced JSON)")]])mode.append(uiOption(label,value));mode.value=definition.source?.roots?'multi':definition.source&&Object.hasOwn(definition.source,'commit')?'git':definition.source?'dirty':'current';modeLabel.append(mode);target.append(modeLabel);
 mode.onchange=()=>{definition.source=mode.value==='git'?{commit:'',patches:[]}:mode.value==='dirty'?{expected_head:null,untracked:[]}:mode.value==='multi'?(definition.source?.roots?definition.source:{roots:[]}):null;update();buildStageEditor();};
 if(mode.value==='git'){
  field(uiMessage("Exact commit SHA"),definition.source.commit,value=>definition.source.commit=value.trim());
  definition.source.patches.forEach((patch,index)=>{field(uiMessage('Patch {0} name',{0:index+1}),patch.name,value=>patch.name=value);field(uiMessage('Patch {0} unified diff',{0:index+1}),patch.diff,value=>patch.diff=value,6);target.append(button(uiMessage("Remove patch {0}",{"0":index+1}),()=>{definition.source.patches.splice(index,1);update();buildStageEditor();}));});
  target.append(button(uiMessage("Add patch"),()=>{definition.source.patches.push({name:'patch-'+(definition.source.patches.length+1),diff:''});update();buildStageEditor();}));
 }
 if(mode.value==='dirty'){field(uiMessage("Expected HEAD (optional)"),definition.source.expected_head,value=>definition.source.expected_head=value.trim()||null);field(uiMessage("Untracked paths — one per line"),(definition.source.untracked||[]).join('\n'),value=>definition.source.untracked=value.split('\n').map(v=>v.trim()).filter(Boolean),3);}
 const stages=(definition.override_definition||saved.definition).pipeline.stages.filter(s=>s.fixtures?.suite?.cases?.length);
 if(stages.length){target.append(el('h3',uiMessage("Fixture tests")));for(const stage of stages){const group=detail(stage.id,'experiment-fixtures:'+stage.id,false),cases=stage.fixtures.suite.cases;
 const selected=new Set(definition.selected_fixtures?.[stage.id]||cases.map(c=>c.id));
 for(const test of cases){const label=el('label'),check=el('input');check.type='checkbox';check.checked=selected.has(test.id);label.append(check,uiNode(test.id));group.append(label);
 check.onchange=()=>{if(check.checked)selected.add(test.id);else selected.delete(test.id);if(!selected.size){selected.add(test.id);check.checked=true;notice(uiError(uiMessage("Select at least one test.")));return;}definition.selected_fixtures??={};if(selected.size===cases.length)delete definition.selected_fixtures[stage.id];else definition.selected_fixtures[stage.id]=[...selected];update();};}target.append(group);}}
 const override=el('section',undefined,'experiment-overrides');override.append(el('h3',uiMessage("Pipeline overrides")));
 if(definition.override_definition){override.append(button(uiMessage("Use original pipeline"),()=>{definition.override_definition=null;update();buildStageEditor();}));renderStageFields(override,definition.override_definition,update,buildStageEditor);}
 else override.append(button(uiMessage("Customize stages"),async()=>{const saved=await api('pipeline-configs?'+query({project_id:state.project,name:definition.pipeline.name,revision:definition.pipeline.revision}));if(!target.isConnected||!$('editor').open)return;definition.override_definition=structuredClone(saved.definition);update();buildStageEditor();}));target.append(override);
}
document.addEventListener('keydown',event=>{if(event.key==='Escape'&&$('layout').classList.contains('mobile-nav-open')){closeMobileMenu();$('sidebar-toggle').focus();}if(event.key==='/'&&!['INPUT','TEXTAREA','SELECT'].includes(document.activeElement?.tagName)&&!document.querySelector('dialog[open]')){event.preventDefault();const search=[...document.querySelectorAll('main input[type=search]')].find(input=>input.getClientRects().length);if(search)search.focus();else{if($('sidebar').inert)$('sidebar-toggle').click();if($('pipeline-filter').getClientRects().length)$('pipeline-filter').focus();}}});

document.addEventListener('click',event=>{document.querySelectorAll('.row-menu[open]').forEach(menu=>{if(!menu.contains(event.target))menu.open=false;});});

function duration(ms){if(!Number.isFinite(ms)||ms<0)return '—';if(ms<1000)return Math.round(ms)+' ms';if(ms<60000)return (ms/1000).toFixed(1)+' s';return Math.floor(ms/60000)+' min '+Math.floor(ms%60000/1000)+' s';}

async function renderSourceChanges(target,source,id,epoch){
 const heading=el('div',undefined,'actions');heading.append(el('h3',uiMessage("Code changes")));target.append(heading);
 const patches=(source.requested?.patches||[]).map(p=>({...p,root:null}));
 for(const root of source.requested?.roots||[])for(const p of root.revision?.patches||[])patches.push({...p,root:root.name});
 if(!patches.length){const empty=el('div',undefined,'empty-state');empty.append(el('strong',uiMessage("No saved patch for this run")),el('p',uiMessage("This run has no recorded text patch. Compare captured inputs below to identify changed files; hashes alone do not contain a code diff.")));target.append(empty);return;}
 const toolbar=el('div',undefined,'actions'),filter=el('input'),body=el('div',undefined,'patch-list');uiAttribute(filter,"placeholder",uiMessage("Filter changed files"));uiAttribute(filter,'aria-label',uiMessage("Filter changed files"));filter.oninput=()=>{for(const file of body.querySelectorAll('.diff-file'))file.hidden=!file.dataset.path.toLowerCase().includes(filter.value.toLowerCase());};toolbar.append(filter,button(uiMessage("Expand files"),()=>body.querySelectorAll('.diff-file').forEach(f=>f.open=true)),button(uiMessage("Collapse files"),()=>body.querySelectorAll('.diff-file').forEach(f=>f.open=false)));target.append(toolbar,body);
 for(const patch of patches){const section=el('section',undefined,'patch-section'),header=el('div',undefined,'actions');header.append(el('h4',(patch.root?patch.root+' / ':'')+patch.name));section.append(header);body.append(section);
  try{const result=await api('run-source-patch?'+query({run_id:id,ordinal:patch.ordinal,root_name:patch.root}));if(epoch!==state.epoch||state.tab!=='Changes')return;const raw=result.patch.diff;header.append(copyButton(raw,uiMessage("Copy patch")));renderUnifiedDiff(section,raw);}catch(error){section.append(el('p',uiMessage("Patch could not be loaded: {0}",{"0":error.message}),'error'));}
 }
}
function renderUnifiedDiff(target,raw){
 const lines=raw.split('\n');let file=null,table=null,oldLine=null,newLine=null,inHunk=false;
 const create=name=>{file=el('details',undefined,'diff-file');file.open=true;file.dataset.path=name;file.append(el('summary',name));table=el('table',undefined,'diff-lines');uiAttribute(table,'aria-label',uiMessage("Code diff for {0}",{"0":name}));file.append(table);target.append(file);};
 for(let i=0;i<lines.length;i++){const line=lines[i];if(i===lines.length-1&&line==='')continue;
  if(line.startsWith('diff --git ')){create(line.slice(11));oldLine=newLine=null;inHunk=false;}
  if(!file)create(uiMessage("Patch"));
  if(!inHunk&&line.startsWith('+++ ')){const name=line.slice(4);if(name!=='/dev/null'){file.dataset.path=name;uiText(file.querySelector('summary'),(name.startsWith('b/')?name.slice(2):name));}}
  const hunk=line.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);let kind='meta',left='',right='';
  if(hunk){oldLine=Number(hunk[1]);newLine=Number(hunk[2]);inHunk=true;kind='hunk';}
  else if(inHunk&&line.startsWith('+')){kind='added';right=newLine++;}
  else if(inHunk&&line.startsWith('-')){kind='removed';left=oldLine++;}
  else if(inHunk&&line.startsWith(' ')){kind='context';left=oldLine++;right=newLine++;}
  const row=el('tr',undefined,'diff-'+kind);row.append(el('td',left,'line-number'),el('td',right,'line-number'),el('td',line,'diff-code'));table.append(row);
 }
}

function hideSettings(){if($('settings-page').open)closeDialog($('settings-page'));settingsActive=false;settingsGeneration++;}
$('settings-page').addEventListener('close',()=>{if($('settings-page').open)return;settingsActive=false;settingsGeneration++;$('settings-content').replaceChildren();$('settings-notice').replaceChildren();if(location.pathname==='/settings')writeRoute(true);if(ciSettingsChanged&&state.section==='ci'){ciSettingsChanged=false;externalCI.show(state.project).catch(notice);}});
$('settings-open').onclick=action(showSettings);
$('settings-back').onclick=hideSettings;
async function showSettings(options={}){
 settingsContext={scope:options.scope==='ci'?'ci':'all',projectId:options.projectId||state.project,add:!!options.add};uiText($('settings-title'),settingsContext.scope==='ci'?'External CI settings':'Settings');
 settingsActive=true;const generation=++settingsGeneration;$('settings-notice').replaceChildren();if(!$('settings-page').open)$('settings-page').showModal();
 const content=$('settings-content');content.replaceChildren(el('p','Checking API access…','muted'));
 const access=await api('capabilities');if(!settingsActive||generation!==settingsGeneration)return;state.apiRole=access.access?.role||'local_owner';content.replaceChildren();
 if(state.browserAuthenticated)content.append(button('Sign out',async()=>{await api('browser-session',undefined,'DELETE');location.reload();}));
 if(state.apiRole&&!['admin','local_owner'].includes(state.apiRole)){content.append(el('p','API role: '+(state.apiRole==='read_only'?'Read only':'Execute')+'. Server administration requires an Admin client credential.','empty-state'));return;}
 const admin=el('section',undefined,'settings-card');admin.append(el('h2',settingsContext.scope==='ci'?'Connect CI providers':uiMessage("Server administration")),el('p',uiMessage("Manage server configuration. Unlock to see which settings can be applied live.")));
 const auth=el('form'),token=el('input');token.type='password';token.autocomplete='off';token.required=true;token.minLength=32;token.id='settings-admin-token';token.setAttribute('aria-describedby','settings-auth-hint');uiAttribute(token,'aria-label',uiMessage("Administrator token"));uiAttribute(token,"placeholder",uiMessage("Administrator token"));const unlock=el('button',uiMessage("Unlock settings"));unlock.type='submit';const authHint=el('p',uiMessage("Format: at least 32 characters, exactly as configured in PREFLIGHT_ADMIN_TOKEN on the server. Kept only in this form, never in browser storage."),'muted');authHint.id='settings-auth-hint';auth.append(token,unlock,authHint);admin.append(auth);content.append(admin);
 auth.onsubmit=action(async event=>{event.preventDefault();const data=await api('server-settings',{admin_token:token.value});if(!settingsActive||generation!==settingsGeneration)return;auth.hidden=true;renderServerSettings(admin,data,token,generation);});
}
function renderServerSettings(target,data,token,generation){
 renderSettings({target,data:{...data,runtime_settings:state.canDocker},scope:settingsContext.scope,projectId:settingsContext.projectId,addCISource:settingsContext.add,token,el,button,api,remoteChoices,isCurrent:()=>settingsActive&&generation===settingsGeneration,
  onSaved:result=>{if(settingsContext.scope==='ci')ciSettingsChanged=true;renderServerSettings(target,result,token,generation);notice(result.reload?.mode==='hot'?"Settings applied. Running pipelines were not interrupted.":"Configuration saved. Restart the server after current work finishes to apply it.");},
  onLock:()=>{token.value='';showSettings(settingsContext);}});settingsContext.add=false;
}

async function renderRunComparison(target,id,epoch){
 const project=state.project,current=()=>epoch===state.epoch&&state.tab==='Changes'&&state.project===project&&target.isConnected;
 const controls=el('div',undefined,'actions'),label=el('label','Compare with'),baseline=el('select'),result=el('div');uiAttribute(baseline,'aria-label','Baseline run');baseline.append(uiOption('Loading project runs…',''));baseline.disabled=true;label.append(baseline);
 let cursor=null,pendingBaseline=state.experimentDefinition?.baseline_run;
 const older=button('Older runs',()=>loadRuns());older.hidden=true;bindPageChoices(older,[baseline]);
 const compare=button(state.canRunCompare?'Compare runs':'Compare captured inputs',async()=>{
  if(!baseline.value)return;compare.disabled=true;result.replaceChildren(el('p','Comparing…','muted'));
  try{
   const selected=baseline.value;
   if(state.canRunCompare){const data=await api('run-compare?'+query({project_id:project,run_id:id,baseline_id:selected,offset:0}));if(!current()||baseline.value!==selected)return;result.replaceChildren();const summary=el('div');result.append(summary);renderSummary(data,summary,selected);}
   else result.replaceChildren();
   const sources=el('div');result.append(sources);
   try{const diff=await api('run-input-diff?'+query({run_id:id,baseline_id:selected,offset:0}));if(current()&&baseline.value===selected)renderInputComparison(sources,diff,id,selected,epoch);}catch(error){if(current())sources.replaceChildren(el('p','Source comparison unavailable: '+error.message,'muted'));}
  }finally{compare.disabled=!baseline.value;}
 });compare.disabled=true;controls.append(label,older,compare);
 if(state.canDiagnostics)controls.append(button('Compiler diagnostics',()=>openDiagnostics({project,runId:id,baselineId:baseline.value||null,api,el,button,code,copy,runLabel,onEvidence:showEvidenceContext,isCurrent:()=>state.project===project})));
 target.append(controls,result);
 const appendRun=row=>{const key=row.run_id||row.id;if(key===id||[...baseline.options].some(o=>o.value===key))return;baseline.append(uiOption(runLabel({...row,run_id:key}),key));};
 async function loadRuns(){older.disabled=true;try{const page=await api('run-history?'+query({project_id:project,include_experiments:true,before:cursor,limit:25}));if(!current())return;if(cursor===null)baseline.replaceChildren(uiOption('Select a baseline run…',''));for(const row of page.runs)appendRun(row);cursor=page.next_cursor;older.hidden=cursor===null;baseline.disabled=baseline.options.length===1;
  if(pendingBaseline){if(![...baseline.options].some(o=>o.value===pendingBaseline)){const run=await api('runs/'+encodeURIComponent(pendingBaseline));if(!current())return;if(run.project_id===project)appendRun(run);}baseline.value=pendingBaseline;pendingBaseline=null;}compare.disabled=!baseline.value;
 }finally{older.disabled=false;}}
 baseline.onchange=()=>{compare.disabled=!baseline.value;result.replaceChildren();};
 function renderSummary(data,body,selected,append=false){
  if(!append){const top=el('div',undefined,'card');top.append(el('strong',data.status.baseline+' → '+data.status.current));if(data.toolchain_equal!==true)top.append(el('p',data.toolchain_equal===false?'Toolchain identities differ.':'Toolchain equivalence is unknown.','muted'));if(!data.test_evidence_complete)top.append(el('p','Per-test evidence is incomplete; comparisons cover recorded cases.','muted'));top.append(button('Copy comparison',()=>copy(data)));body.append(top);}
  for(const [key,title,total]of [['stage_changes','Stage changes','stage_changes_total'],['test_changes','Test changes','test_changes_total'],['artifact_changes','Artifact changes','artifact_changes_total']]){if(!data[key]?.length)continue;const panel=el('section',undefined,'card');panel.append(el('h3',title+' · '+data[total]));for(const change of data[key]){const row=el('div',undefined,'comparison-result');row.append(el('strong',change.name||change.stage_id||change.test||change.artifact),copyField(el('code',String(change.baseline??'—')),change.baseline??''),el('span','→','muted'),copyField(el('code',String(change.current??'—')),change.current??''));panel.append(row);}body.append(panel);}
  if(data.next_cursor!==null){const more=button('More result changes',async()=>{const page=await api('run-compare?'+query({project_id:project,run_id:id,baseline_id:selected,offset:data.next_cursor}));if(current()&&baseline.value===selected){more.remove();renderSummary(page,body,selected,true);}});body.append(more);}
 }
 await loadRuns();
}

function renderInputComparison(target,result,id,baseline,epoch,append=false){
 if(!append)target.replaceChildren();const inputs=result.inputs||result,diff=inputs.diff;
 if(!diff){target.append(facts(result));return;}
 if(!append)target.append(el('h3',uiMessage("{0} changed paths",{"0":diff.total_changes})),el('p',(diff.configuration_changed?uiMessage("Run configuration also changed."):uiMessage("Run configuration unchanged.")),'muted'));
 for(const item of diff.changes){const file=el('details',undefined,'comparison-file'),summary=el('summary',item.path+' · '+item.change),body=el('div');file.append(summary,body);target.append(file);let loaded=false;
  file.addEventListener('toggle',async()=>{if(!file.open||loaded)return;loaded=true;body.replaceChildren(el('p',uiMessage("Loading captured source…"),'muted'));try{const text=await api('run-source-diff?'+query({project_id:state.project,run_id:id,baseline_id:baseline,path:item.path}));if(epoch!==state.epoch||state.tab!=='Changes')return;body.replaceChildren();if(text.diff){body.append(copyButton(text.diff,uiMessage("Copy file diff")));renderUnifiedDiff(body,text.diff);}else body.append(el('p',(!text.before_exists?uiMessage("Empty file added."):(!text.after_exists?uiMessage("Empty file removed."):uiMessage("No text changes (metadata or permissions changed).")))));}catch(error){body.replaceChildren(el('p',uiMessage("Code diff unavailable: {0}",{"0":error.message}),'muted'));}});
 }
 if(diff.next_cursor!==null){const more=button(uiMessage("More changed files"),async()=>{const page=await api('run-input-diff?'+query({run_id:id,baseline_id:baseline,offset:diff.next_cursor}));if(epoch!==state.epoch||state.tab!=='Changes')return;more.remove();renderInputComparison(target,page,id,baseline,epoch,true);});target.append(more);}
 if(result.inputs){const other=detail(uiMessage("Test and artifact comparison"),'experiment-comparison',false);other.append(facts({...result,inputs:undefined}));target.append(other);}
}

window.addEventListener('resize',()=>document.querySelectorAll('.row-menu[open]').forEach(menu=>menu.open=false));

// Give icon controls and placeholder-only fields an accessible name.
for(const input of document.querySelectorAll('input[placeholder],textarea[placeholder]'))if(!input.labels?.length&&!input.hasAttribute('aria-label'))uiAttribute(input,'aria-label',input.placeholder);

function revealSelectedTab(){const tab=$('tabs').querySelector('[aria-selected=true]');if(tab)$('tabs').scrollLeft=tab.offsetLeft-$('tabs').offsetLeft-($('tabs').clientWidth-tab.offsetWidth)/2;}
new ResizeObserver(revealSelectedTab).observe($('tabs'));

function renderFixtureCase(result,definition){
 const target=$('test-content'),summary=el('div',undefined,'actions');summary.append(badge(result.status));if(result.started_at&&result.finished_at)summary.append(el('span',duration(result.finished_at-result.started_at),'muted'));target.replaceChildren(summary);
 if(result.reason)target.append(el('p',result.reason,'failure-summary'));
 if(result.checks?.length){target.append(el('h3',uiMessage("Checks")));for(const check of result.checks){const row=el('details',undefined,'fixture-check');row.open=!check.passed;const title=el('summary');title.append(badge(check.passed?'passed':'failed'),uiNode(check.target));row.append(title,el('h4',uiMessage("Expected")),code(check.expected),el('h4',uiMessage("Actual")),code(check.actual));if(check.diff)row.append(el('h4',uiMessage("Diff")),code(check.diff));target.append(row);}}
 const files=Object.entries(definition?.files||{});if(files.length){target.append(el('h3',uiMessage("Source files")));for(const [path,content]of files){const file=el('section',undefined,'fixture-source');file.append(el('h4',path),code(content));target.append(file);}}
 if(definition?.stdin){const input=detail(uiMessage("Standard input"),'fixture-stdin',false);input.append(code(definition.stdin));target.append(input);}
 const raw=detail(uiMessage("Full result and definition"),'fixture-raw',false);raw.append(code({result,definition}));target.append(raw);
}

function renderTestConfiguration(target,stage,update,rebuild){
 const label=el('label',uiMessage("Stage type")),kind=el('select');uiAttribute(kind,'aria-label',uiMessage("Stage type for {0}",{"0":stage.id}));kind.append(uiOption(uiMessage("Build"),'build'));for(const [value,title]of testKinds)kind.append(uiOption(title,value));const current=stage.kind==='test'?(stage.testing?.kind||(stage.fixtures?'fixture':'custom')):'build';if(![...kind.options].some(o=>o.value===current))kind.append(uiOption(current,current));kind.value=current;if(stage.fixtures)kind.options[0].disabled=true;label.append(kind);target.append(label);
 kind.onchange=()=>{stage.kind=kind.value==='build'?'build':'test';if(stage.kind==='build'){delete stage.testing;stage.junit=[];}else{stage.testing??={kind:kind.value,reports:[]};stage.testing.kind=kind.value;}update();rebuild();};
 if(stage.kind!=='test')return;
 if(stage.fixtures){target.append(el('p',uiMessage("Results: native fixture checks"),'muted'));return;}
 const formatLabel=el('label',uiMessage("Result adapter")),format=el('select');uiAttribute(format,'aria-label',uiMessage("Result adapter for {0}",{"0":stage.id}));for(const [value,title]of [['none',uiMessage("Command only")],['auto',uiMessage("Auto: JUnit or Preflight JSON")],['junit','JUnit XML'],['preflight_json',uiMessage("Preflight JSON")]])format.append(uiOption(title,value));
 const paths=[...(stage.junit||[]),...(stage.testing?.reports||[]).map(r=>r.path)];format.value=paths.length?(stage.junit?.length?'junit':stage.testing.reports[0].format):'none';formatLabel.append(format);target.append(formatLabel);
 const reportLabel=el('label',uiMessage("Report files · one path per line")),reports=el('textarea');reports.rows=2;reports.value=paths.join('\n');reports.disabled=format.value==='none';reportLabel.append(reports);target.append(reportLabel);
 const write=()=>{stage.junit=[];stage.testing??={kind:current,reports:[]};stage.testing.reports=format.value==='none'?[]:reports.value.split('\n').map(p=>p.trim()).filter(Boolean).map(path=>({format:format.value,path}));update();};format.onchange=()=>{reports.disabled=format.value==='none';write();};reports.oninput=write;
 if(!paths.length)target.append(el('p',uiMessage("Add a report to record individual test results."),'muted'));
}
function renderAcceptanceEditor(target,definition,update,rebuild){
 const section=el('section',undefined,'acceptance-editor');section.append(el('h3',uiMessage("Acceptance criteria")));const criteria=definition.pipeline.acceptance||[],stages=definition.pipeline.stages;
 const choice=(parent,label,items,value,write)=>{const wrapper=el('label',label),select=el('select');uiAttribute(select,'aria-label',uiMessage("{0} for {1}",{"0":label,"1":parent.dataset.criterion}));for(const [v,text]of items)select.append(uiOption(text,v));select.value=value??'';select.disabled=!select.options.length;select.onchange=()=>{write(select.value);update();rebuild();};wrapper.append(select);parent.append(wrapper);return select;};
 const field=(parent,label,value,write,type='text')=>{const wrapper=el('label',label),input=el('input');input.type=type;input.value=value;input.oninput=()=>{write(type==='number'?Number(input.value):input.value);update();};wrapper.append(input);parent.append(wrapper);return input;};
 const defaults=kind=>kind==='stage_succeeded'?{kind,stage_id:stages[0]?.id||''}:kind==='tests'?{kind,stage_id:stages.find(s=>s.kind==='test')?.id||null,min_passed:1,max_failed:0,require_complete:true}:kind==='artifact'?{kind,stage_id:stages.find(s=>Object.keys(s.outputs||{}).length)?.id||stages[0]?.id||'',name:'',min_bytes:1}:kind==='duration'?{kind,stage_id:null,max_ms:60000}:{kind,stage_id:null,text:'',must_exist:true};
 for(const criterion of criteria){const card=el('details',undefined,'stage-form');card.open=true;card.append(el('summary',criterion.id));const fields=el('div',undefined,'stage-fields'),rule=criterion.rule;fields.dataset.criterion=criterion.id;field(fields,uiMessage("Criterion name"),criterion.id,value=>criterion.id=value);
 choice(fields,uiMessage("Check"),[['stage_succeeded',uiMessage("Stage succeeds")],['tests',uiMessage("Test counts and completeness")],['artifact',uiMessage("Artifact is published")],['duration',uiMessage("Duration limit")],['log_text',uiMessage("Log contains / excludes text")]],rule.kind,value=>criterion.rule=defaults(value));
 const applicable=rule.kind==='tests'?stages.filter(s=>s.kind==='test'):rule.kind==='artifact'?stages.filter(s=>Object.keys(s.outputs||{}).length):stages;
 choice(fields,uiMessage("Scope"),[...(['tests','duration','log_text'].includes(rule.kind)?[['',uiMessage("All applicable stages")]]:[]),...applicable.map(s=>[s.id,s.id])],rule.stage_id||'',value=>rule.stage_id=value||null);
 if(rule.kind==='tests'){field(fields,uiMessage("Minimum passed"),rule.min_passed??0,v=>rule.min_passed=v,'number').min='0';field(fields,uiMessage("Maximum failed"),rule.max_failed??0,v=>rule.max_failed=v,'number').min='0';const label=el('label'),check=el('input');check.type='checkbox';check.checked=rule.require_complete!==false;check.onchange=()=>{rule.require_complete=check.checked;update();};label.append(check,uiNode(uiMessage('Require complete case evidence')));fields.append(label);}
 if(rule.kind==='artifact'){const outputs=stages.find(s=>s.id===rule.stage_id)?.outputs||{};if(!Object.hasOwn(outputs,rule.name)&&Object.keys(outputs).length){rule.name=Object.keys(outputs)[0];update();}choice(fields,uiMessage("Artifact"),Object.keys(outputs).map(name=>[name,name]),rule.name,v=>rule.name=v);if(!Object.keys(outputs).length)fields.append(el('p',uiMessage("Declare a published artifact in a stage first."),'muted'));field(fields,uiMessage("Minimum bytes"),rule.min_bytes??0,v=>rule.min_bytes=v,'number').min='0';}
 if(rule.kind==='duration'){field(fields,uiMessage("Maximum elapsed milliseconds"),rule.max_ms,v=>rule.max_ms=v,'number').min='1';}
 if(rule.kind==='log_text'){field(fields,uiMessage("Exact text"),rule.text,v=>rule.text=v);choice(fields,uiMessage("Expectation"),[['true',uiMessage("Must be present")],['false',uiMessage("Must be absent")]],String(rule.must_exist!==false),v=>rule.must_exist=v==='true');}
 fields.append(button(uiMessage("Remove criterion"),()=>{definition.pipeline.acceptance=criteria.filter(c=>c!==criterion);update();rebuild();}));card.append(fields);section.append(card);
 }
 if(!criteria.length)section.append(el('p',uiMessage("No additional checks. Execution status still applies."),'muted'));
 section.append(el('p',uiMessage("Execution failures still fail the run."),'muted'));
 section.append(button(uiMessage("Add criterion"),()=>{definition.pipeline.acceptance??=[];let index=criteria.length+1;while(criteria.some(c=>c.id==='criterion-'+index))index++;definition.pipeline.acceptance.push({id:'criterion-'+index,rule:defaults('stage_succeeded')});update();rebuild();}));target.append(section);
}
async function renderAcceptance(target,id,epoch){
 const report=await api('runs/'+encodeURIComponent(id)+'/acceptance');if(epoch!==state.epoch||state.tab!=='Acceptance')return;
 const head=el('div',undefined,'heading');head.append(el('h3',uiMessage("Acceptance")),badge(report.status));target.append(head);
 if(!report.criteria?.length){target.append(el('p',(report.status==='not_evaluated'?uiMessage("Acceptance has not been evaluated."):uiMessage("No acceptance criteria configured for this run.")),'empty-state'));if(state.definition)target.append(button(uiMessage("Configure future runs"),()=>openEditor()));return;}
 for(const result of report.criteria){const card=el('article',undefined,'card'),top=el('div',undefined,'heading');top.append(el('strong',result.id),badge(result.status));card.append(top,el('p',criterionExpectation(result.rule),'muted'),facts(result.actual));const rule=detail(uiMessage("Recorded rule"),id+':criterion:'+result.id,false);rule.append(code(result.rule));card.append(rule);target.append(card);}
}
function openEvidence(seed={}){if($('test-dialog').open)closeDialog($('test-dialog'));state.evidenceSeed=seed;showSection('evidence');}
function evidenceCard(hit){
 const card=el('article',undefined,'card evidence-card'),heading=el('div',undefined,'heading');heading.append(el('strong',(hit.pipeline_name||'Run '+hit.run_id.slice(-8))+' / '+hit.stage_id),badge(hit.kind));card.append(heading,el('p',uiMessage("{0} · {1}{2}",{"0":uiFormatted(new Date(hit.timestamp),"toLocaleString"),"1":hit.stream,"2":(hit.test_id?uiMessage(" · test output"):'')}),'muted'),code(hit.excerpt));
 const actions=el('div',undefined,'actions');actions.append(button(uiMessage("Context"),()=>showEvidenceContext(hit)),button(uiMessage("Open run"),async()=>{const entry=state.entries.find(p=>p.pipeline_key===hit.pipeline_key)||{pipeline_key:hit.pipeline_key,name:hit.pipeline_name||'Run'};await selectPipeline(entry);await selectRun(hit.run_id);state.stage=hit.stage_id;state.tab='Logs';renderTabs();await renderView();}));card.append(actions);return card;
}
async function showEvidenceContext(hit){
 const project=state.project,result=await api('evidence/read?'+query({project_id:project,evidence_id:hit.evidence_id,neighbors:2}));if(state.project!==project)return;const dialog=el('dialog');uiAttribute(dialog,'aria-label',uiMessage("Log context"));dialog.append(el('h2',uiMessage("Log context")),el('p',result.stage_id+' · '+result.stream,'muted'));for(const event of result.events){dialog.append(code(event.text));if(event.truncated&&state.canByteWindows)dialog.append(button('Read full event',()=>showEvidenceWindow(event.evidence_id)));}const metadata=detail(uiMessage("Evidence reference"),hit.evidence_id,false);const {events,...reference}=result;metadata.append(facts(reference));dialog.append(metadata,button(uiMessage("Copy context"),()=>copy(result)),modalClose(button(uiMessage("Close"),()=>closeDialog(dialog))));dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();
}
async function showEvidenceWindow(evidenceId){
 const project=state.project,dialog=el('dialog'),content=el('div'),status=el('p','Loading…','muted');let offset=0,text='',reading=false;
 uiAttribute(dialog,'aria-label','Log event');const heading=el('div',undefined,'heading');heading.append(el('h2','Log event'),modalClose(button('Close',()=>closeDialog(dialog))));
 const more=button('More text',async()=>{if(reading||offset===null)return;reading=true;more.disabled=true;try{const page=await api('evidence/read?'+query({project_id:project,evidence_id:evidenceId,neighbors:0,offset,max_bytes:16384}));if(!dialog.open||state.project!==project)return;text+=page.text;offset=page.next_offset;const scroll=content.scrollTop;content.replaceChildren(code(text),more);content.scrollTop=scroll;status.textContent=offset===null?'End of event':formatBytes(new TextEncoder().encode(text).length)+' loaded';more.hidden=offset===null;}finally{reading=false;more.disabled=false;}});
 content.classList.add('evidence-text-window');content.append(more);dialog.append(heading,status,content);dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();await more.onclick();
}
async function renderEvidence(){
 const target=$('evidence-panel');state.disposeEvidence?.();state.disposeEvidence=null;target.replaceChildren();const body=el('div');target.append(body);const project=state.project,seed=state.evidenceSeed||{},form=el('form'),filters=el('div',undefined,'evidence-filters'),search=el('input'),stats=el('p',uiMessage("Loading index…"),'muted'),results=el('div');search.type='search';uiAttribute(search,"placeholder",uiMessage("Search build logs and test output…"));uiAttribute(search,'aria-label',uiMessage("Search evidence"));search.value=seed.query||'';
 const choice=(name,items)=>{const label=el('label',name),select=el('select');uiAttribute(select,'aria-label',name);for(const [value,text]of items)select.append(uiOption(text,value));label.append(select);filters.append(label);return select;};
 const run=choice(uiMessage("Run"),[['',uiMessage("All project runs")]]),stage=choice(uiMessage("Stage"),[['',uiMessage("All stages")]]),test=choice(uiMessage("Test"),[['',uiMessage("All tests")]]),kind=choice(uiMessage("Evidence type"),[['',uiMessage("All types")],['build',uiMessage("Build")],...testKinds]),stream=choice(uiMessage("Output"),[['',uiMessage("All streams")],['stdout',uiMessage("Standard output")],['stderr',uiMessage("Standard error")],['diagnostic',uiMessage("Test diagnostics")],['fixture',uiMessage("Fixture events")],['lifecycle',uiMessage("Execution events")]]);stage.disabled=test.disabled=true;
 const severity=choice('Severity',[['','All severities'],['error','Errors'],['warning','Warnings']]);
 const extra=(label,type='text')=>{const box=el('label',label),input=el('input');input.type=type;uiAttribute(input,'aria-label',label);box.append(input);filters.append(box);box.hidden=!state.canEvidenceFilters;return input;};
 const file=extra('Source file'),sha=extra('Git commit'),from=extra('From (local time)','datetime-local'),to=extra('Before (local time)','datetime-local');severity.parentElement.hidden=!state.canEvidenceFilters;sha.placeholder='Full commit SHA';file.placeholder='src/main.rs';from.step=to.step='1';
 const bar=el('div',undefined,'actions'),submit=el('button',uiMessage("Search"),'primary'),copyContext=button(uiMessage("Copy retrieval context"),async()=>{const data=await api('evidence/context?'+query(parameters()));await copy(data);});submit.type='submit';copyContext.disabled=true;const diagnostics=button('Compiler diagnostics',()=>openDiagnostics({project,runId:run.value,stageId:stage.value,api,el,button,code,copy,runLabel,onEvidence:showEvidenceContext,isCurrent:()=>state.project===project}));diagnostics.hidden=!state.canDiagnostics;bar.append(search,submit,copyContext,diagnostics);form.append(bar,filters);form.inert=true;body.append(stats,form,results);
 const [index,history]=await Promise.all([api('evidence/stats?'+query({project_id:project})),api('run-history?'+query({project_id:project,limit:100}))]);if(!body.isConnected||state.project!==project)return;uiText(stats,uiMessage("{0} indexed events · {1} runs · {2}",{"0":index.events,"1":index.runs,"2":formatBytes(index.indexed_bytes)}));for(const entry of history.runs)run.append(uiOption(runLabel(entry),entry.run_id));
 if(seed.run_id&&![...run.options].some(o=>o.value===seed.run_id)){const selected=await api('runs/'+encodeURIComponent(seed.run_id));if(!body.isConnected)return;run.append(uiOption(runLabel({run_id:selected.id,...selected}),selected.id));}run.value=seed.run_id||'';
 let historyCursor=history.next_cursor;const olderRuns=button('Older runs',async()=>{const page=await api('run-history?'+query({project_id:project,before:historyCursor,limit:100}));if(!body.isConnected)return;for(const entry of page.runs)if(![...run.options].some(o=>o.value===entry.run_id))run.append(uiOption(runLabel(entry),entry.run_id));historyCursor=page.next_cursor;olderRuns.hidden=!historyCursor;});olderRuns.hidden=!historyCursor;filters.append(olderRuns);bindPageChoices(olderRuns,[run]);
 let before=null,testsOffset=null,evidenceGeneration=0,liveRun=false,indexingPending=false;const moreTests=button(uiMessage("More test choices"),()=>loadTestChoices(true));moreTests.hidden=true;filters.append(moreTests);bindPageChoices(moreTests,[test]);
 function parameters(){const selected=test.value?JSON.parse(test.value):null;return {project_id:project,severity:severity.value||null,file:file.value.trim()||null,git_sha:sha.value.trim()||null,from_ms:from.value?new Date(from.value).getTime():null,to_ms:to.value?new Date(to.value).getTime():null,query:search.value,run_id:run.value||null,stage_id:stage.value||selected?.[0]||null,test_id:selected?.[1]||null,kind:kind.value||null,stream:stream.value||null,before,limit:20};}
 let evidenceRows=[],evidenceList=null;let evidenceLoading=false;const evidencePager=mountInfiniteScroll(target,()=>load(before===null),{host:body});state.disposeEvidence=()=>{evidencePager.destroy();evidenceList?.destroy();};
 async function load(reset=true){if(!body.isConnected||(!reset&&(evidenceLoading||!before)))return;const generation=++evidenceGeneration;evidenceLoading=true;try{if(reset){evidencePager.set({more:false});before=null;evidenceList?.destroy();evidenceList=null;evidenceRows=[];results.replaceChildren(el('p',uiMessage("Searching…"),'muted'));}const page=await api('evidence/search?'+query(parameters()));if(!body.isConnected||state.project!==project||generation!==evidenceGeneration)return;if(reset)results.replaceChildren();if(!evidenceList)evidenceList=mountVirtualList(target,{host:results,render:evidenceCard,key:hit=>hit.cursor,estimate:270});const seen=new Set(evidenceRows.map(hit=>hit.cursor));evidenceRows.push(...page.matches.filter(hit=>!seen.has(hit.cursor)));evidenceList.setItems(evidenceRows);before=page.next_cursor;if(page.expired_log_runs)results.prepend(el('p',`${page.expired_log_runs} runs in this workspace have expired log payloads; metadata remains available.`,'muted'));indexingPending=page.diagnostic_filter_complete===false;evidencePager.set({more:before});copyContext.disabled=!evidenceRows.length;if(indexingPending)results.append(el('p','Diagnostic indexing is still in progress; filtered results are incomplete.','muted'));if(!evidenceRows.length){const empty=el('div',(test.value?uiMessage("No case-scoped output recorded for these filters."):uiMessage("No matching evidence. Adjust the search or filters.")),'empty-state');if(test.value)empty.append(button(uiMessage("Search stage output"),async()=>{const selected=JSON.parse(test.value);stage.value=selected[0];test.value='';syncSelects();await load();}));results.append(empty);}}catch(error){if(generation===evidenceGeneration&&body.isConnected){evidencePager.set({more:true,error:true});if(reset)results.replaceChildren(el('p','Unable to load evidence.','muted'));}throw error;}finally{if(generation===evidenceGeneration)evidenceLoading=false;}}
 async function loadTestChoices(append=false){if(!run.value)return;const selectedRun=run.value,selectedStage=stage.value,page=await api('test-results?'+query({project_id:project,run_id:selectedRun,offset:append?testsOffset:0}));if(!body.isConnected||run.value!==selectedRun||stage.value!==selectedStage)return;if(!append)test.replaceChildren(uiOption(uiMessage("All tests"),''));for(const item of page.cases.filter(c=>!stage.value||c.stage_id===stage.value))test.append(uiOption(item.stage_id+' / '+item.case.name,JSON.stringify([item.stage_id,item.case.id])));testsOffset=page.next_cursor;moreTests.hidden=testsOffset===null;test.disabled=false;}
 async function loadRunChoices(useSeed=false){stage.replaceChildren(uiOption(uiMessage("All stages"),''));test.replaceChildren(uiOption(uiMessage("All tests"),''));stage.disabled=test.disabled=!run.value;moreTests.hidden=true;liveRun=false;if(!run.value)return;const selected=run.value,data=await api('runs/'+encodeURIComponent(selected));if(!body.isConnected||run.value!==selected)return;liveRun=!terminal.has(data.status);for(const item of data.stages){stage.append(uiOption(item.definition.id,item.definition.id));const type=item.definition.testing?.kind;if(type&&![...kind.options].some(o=>o.value===type))kind.append(uiOption(testKindLabel(type),type));}if(useSeed)stage.value=seed.stage_id||'';await loadTestChoices();if(useSeed&&seed.test_id){const selected=JSON.stringify([seed.stage_id,seed.test_id]);if(![...test.options].some(o=>o.value===selected))test.append(uiOption((seed.test_name || uiMessage("Selected test")),selected));test.value=selected;}}
 form.onsubmit=action(async event=>{event.preventDefault();await load();});run.onchange=action(async()=>{await loadRunChoices();await load();});stage.onchange=action(async()=>{await loadTestChoices();await load();});test.onchange=kind.onchange=stream.onchange=severity.onchange=file.onchange=sha.onchange=from.onchange=to.onchange=action(()=>load());try{await loadRunChoices(true);await load();}finally{form.inert=false;}
 async function followEvidence(){if(!body.isConnected||state.section!=='evidence'||state.project!==project)return;try{if((liveRun||indexingPending)&&target.scrollTop<100&&evidenceRows.length<=20&&!document.hidden&&!form.contains(document.activeElement)&&!document.querySelector('dialog[open]')&&getSelection().isCollapsed){const selected=run.value;if(selected){const data=await api('runs/'+encodeURIComponent(selected));if(run.value===selected){liveRun=!terminal.has(data.status);await load();}}else await load();}}catch(error){uiAttribute(stats,"title",error.message);}setTimeout(followEvidence,3000);}setTimeout(followEvidence,3000);
}

function renderStageOutputs(target,stage,update,rebuild){
 const panel=el('details',undefined,'stage-outputs');panel.append(el('summary',uiMessage("Published artifacts")));
 for(const [name,output]of Object.entries(stage.outputs||{})){let key=name;const fields=el('div',undefined,'stage-fields'),nameLabel=el('label',uiMessage("Name")),input=el('input');input.value=name;input.onchange=()=>{const next=input.value.trim();if(next!==key&&Object.hasOwn(stage.outputs,next)){input.value=key;notice(uiError(uiMessage("Artifact name already exists.")));return;}delete stage.outputs[key];stage.outputs[next]=output;key=next;update();};nameLabel.append(input);
 const pathLabel=el('label',uiMessage("Relative path")),path=el('input');path.value=output.path;path.onchange=()=>{output.path=path.value;update();};pathLabel.append(path);
 const typeLabel=el('label',uiMessage("Type")),type=el('select');uiAttribute(type,'aria-label',uiMessage("Artifact type for {0}/{1}",{"0":stage.id,"1":name}));for(const [value,label]of [['file',uiMessage("File")],['executable',uiMessage("Executable")],['directory',uiMessage("Directory")]])type.append(uiOption(label,value));type.value=output.type;type.onchange=()=>{output.type=type.value;update();};typeLabel.append(type);fields.append(nameLabel,pathLabel,typeLabel,button(uiMessage("Remove artifact"),()=>{delete stage.outputs[key];update();rebuild();}));panel.append(fields);}
 panel.append(button(uiMessage("Add artifact"),()=>{stage.outputs??={};let i=Object.keys(stage.outputs).length+1;while(Object.hasOwn(stage.outputs,'output-'+i))i++;stage.outputs['output-'+i]={path:'output-'+i+'.bin',type:'file'};update();rebuild();}));target.append(panel);
}

function criterionExpectation(rule){
 if(rule.kind==='stage_succeeded')return uiMessage('{0} must succeed',{0:rule.stage_id});
 if(rule.kind==='tests')return uiMessage('Passed ≥ {0} · failed ≤ {1}{2}',{0:rule.min_passed||0,1:rule.max_failed||0,2:rule.require_complete?uiMessage(' · complete case list required'):''});
 if(rule.kind==='artifact')return uiMessage('{0} · minimum {1} bytes',{0:rule.name,1:rule.min_bytes||0});
 if(rule.kind==='duration')return uiMessage('Maximum elapsed {0} ms',{0:rule.max_ms});
 return uiMessage(rule.must_exist?'Must contain: {0}':'Must exclude: {0}',{0:rule.text});
}

function editorDirty(){return $('definition').value!==editorBase||$('pipeline-name').value!==editorBaseName;}
function updateEditorStatus(){if(state.editorKind!=='pipeline')return;const dirty=editorDirty();uiText($('editor-state'),uiMessage("{0} · {1} bytes",{"0":(dirty?uiMessage("Unsaved changes"):($('pipeline-name').value?uiMessage("Saved revision {0}",{"0":(state.revision || uiMessage("new"))}):uiMessage("New pipeline"))),"1":new TextEncoder().encode($('definition').value).length}));$('editor-revert').disabled=!dirty;$('save').disabled=!dirty||new TextEncoder().encode($('definition').value).length>65536;}
function requestEditorClose(){if(state.editorKind==='pipeline'&&editorDirty()&&!confirm(uiMessage("Discard unsaved pipeline edits?")))return;closeDialog($('editor'));}
function setEditorMode(mode){editorMode=mode;$('pipeline-code-panel').hidden=mode!=='code';$('stage-editor').hidden=mode==='code';$('editor-visual').setAttribute('aria-pressed',String(mode==='visual'));$('editor-code').setAttribute('aria-pressed',String(mode==='code'));if(mode==='visual'){try{JSON.parse($('definition').value);buildStageEditor();}catch(error){notice(uiError(uiMessage("Fix JSON syntax before opening the visual editor.")));setEditorMode('code');return;}}else codeEditor?.view.requestMeasure();}
async function prepareCodeEditor(fresh=false){
 const generation=++editorGeneration;codeEditor?.destroy();codeEditor=null;editorBase=$('definition').value;editorBaseName=$('pipeline-name').value;const pipeline=state.editorKind==='pipeline';$('pipeline-editor-tools').hidden=!pipeline;$('pipeline-code-panel').hidden=!pipeline;$('editor-state').hidden=!pipeline;$('advanced-editor').hidden=pipeline;$('stage-editor').hidden=pipeline;$('editor-revisions').disabled=!$('pipeline-name').value||!state.pipeline?.saved_name;$('editor-problems').replaceChildren();$('pipeline-code-editor').replaceChildren();if(!pipeline){$('save').disabled=false;return;}
 uiText($('pipeline-code-editor'),uiMessage("Loading editor…"));updateEditorStatus();setEditorMode('code');
 try{editorModule??=import('/editor-bundle.mjs');editorSchema??=api('pipeline-schema');const [module,schema]=await Promise.all([editorModule,editorSchema]);if(generation!==editorGeneration||!$('editor').open)return;$('pipeline-code-editor').replaceChildren();
 codeEditor=module.mountPipelineEditor($('pipeline-code-editor'),{phrases:{},labels:editorLabels(),validateOnOpen:!fresh,text:$('definition').value,schema,onChange:text=>{$('definition').value=text;updateEditorStatus();},check:text=>api('pipeline-editor/check',{text}),onDiagnostics:diagnostics=>{if(generation!==editorGeneration)return;const target=$('editor-problems');target.replaceChildren();for(const item of diagnostics)target.append(button(item.message,()=>codeEditor?.goto(item.from)));if(!diagnostics.length){target.append(el('span',uiMessage("No problems"),'muted'));notifications.querySelectorAll('.toast.error[data-context=editor]').forEach(toast=>toast.remove());placeNotifications();}}});if(fresh)$('editor-problems').append(el('span','Set the build command to get started.','muted'));codeEditor.view.requestMeasure();
 }catch(error){$('pipeline-code-editor').replaceChildren(el('p',error.message),button(uiMessage("Retry editor"),()=>prepareCodeEditor(fresh)));setEditorMode('visual');}
}
$('editor-code').onclick=()=>setEditorMode('code');$('editor-visual').onclick=()=>setEditorMode('visual');$('editor-undo').onclick=()=>codeEditor?.undo();$('editor-redo').onclick=()=>codeEditor?.redo();$('editor-suggest').onclick=()=>{setEditorMode('code');codeEditor?.suggest();};$('editor-format').onclick=action(()=>{const formatted=pretty(JSON.parse($('definition').value));$('definition').value=formatted;codeEditor?.setText(formatted);updateEditorStatus();if(editorMode==='visual')buildStageEditor();});$('pipeline-name').oninput=()=>{$('editor-revisions').disabled=$('pipeline-name').value!==state.pipeline?.saved_name;updateEditorStatus();};$('editor-revert').onclick=()=>{$('pipeline-name').value=editorBaseName;$('definition').value=editorBase;codeEditor?.setText(editorBase);updateEditorStatus();if(editorMode==='visual')buildStageEditor();};
$('editor-revisions').onclick=action(async()=>{
 const project=state.project,name=state.pipeline?.saved_name;if(!name)return;const dialog=el('dialog');uiAttribute(dialog,'aria-label',uiMessage("Pipeline revisions"));const list=el('div');let before=null;dialog.append(el('h2',uiMessage("{0} revisions",{"0":name})),el('p',uiMessage("Restoring creates a new revision. Existing runs keep their definitions."),'muted'),modalClose(button(uiMessage("Close"),()=>closeDialog(dialog))),list);
 const more=button(uiMessage("Older revisions"),async()=>{const page=await api('pipeline-editor/revisions?'+query({project_id:project,name,before,limit:25}));if(!dialog.open)return;for(const item of page.items){const card=el('article',undefined,'revision-row');card.append(el('strong',uiMessage("Revision {0}",{"0":item.revision})),el('code',item.sha256.slice(0,12)),button(uiMessage("Load draft"),async()=>{const saved=await api('pipeline-configs?'+query({project_id:project,name,revision:item.revision}));if(state.project!==project||!$('editor').open)return;$('definition').value=pretty(saved.definition);codeEditor?.setText($('definition').value);updateEditorStatus();if(editorMode==='visual')buildStageEditor();closeDialog(dialog);}),button(uiMessage("Restore as new revision"),async()=>{if(editorDirty()&&!confirm(uiMessage("Replace the unsaved draft with this revision?")))return;const saved=await api('pipeline-editor/restore',{project_id:project,name,source_revision:item.revision,expected_revision:page.current_revision});closeDialog(dialog);await selectProject(project);await selectPipeline({pipeline_key:'saved:'+name,name,saved_name:name,revision:saved.revision});notice(uiMessage("Restored revision {0} as revision {1}",{"0":item.revision,"1":saved.revision}));}));list.append(card);}before=page.next_cursor;more.hidden=!before;});dialog.append(more);dialog.addEventListener('close',()=>dialog.remove());document.body.append(dialog);dialog.showModal();await more.onclick();
});

$('editor').addEventListener('keydown',event=>{if((event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='s'){event.preventDefault();$('save').click();}});uiAttribute($('save'),"title",uiMessage("Save revision (⌘S / Ctrl+S)"));

const editorToolMedia=matchMedia('(max-width:600px)');
function layoutEditorTools(){const extra=$('editor-extra-tools');if(extra.matches(':popover-open'))extra.hidePopover();if(editorToolMedia.matches)extra.popover='auto';else extra.removeAttribute('popover');}
editorToolMedia.addEventListener('change',layoutEditorTools);layoutEditorTools();
$('editor-more').onclick=()=>{const extra=$('editor-extra-tools');if(extra.matches(':popover-open')){extra.hidePopover();return;}extra.showPopover();const r=$('editor-more').getBoundingClientRect(),box=extra.getBoundingClientRect();extra.style.left=Math.max(8,Math.min(r.right-box.width,innerWidth-box.width-8))+'px';extra.style.top=Math.max(8,Math.min(r.bottom+6,innerHeight-box.height-8))+'px';};
$('editor-extra-tools').addEventListener('toggle',event=>$('editor-more').setAttribute('aria-expanded',String(event.newState==='open')));
$('editor-extra-tools').addEventListener('click',event=>{if(event.target.closest('button')&&$('editor-extra-tools').matches(':popover-open'))$('editor-extra-tools').hidePopover();});

function editorLabels(){return {editor:uiMessage('Pipeline JSON editor'),tooLarge:uiMessage('Definition exceeds 64 KiB. Format or shorten it before saving.'),validationUnavailable:error=>String(uiMessage('Validation unavailable: {0}',{0:error})),configuration:uiMessage('configuration')};}
