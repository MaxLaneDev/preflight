import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';

const app=readFileSync(new URL('../src/app.js',import.meta.url),'utf8');
const extract=(start,end)=>app.slice(app.indexOf(start),app.indexOf(end,app.indexOf(start)));

test('focused search fields do not stop polling and SSE bypasses catalog throttling',async()=>{
 const calls=[],nodes={explorer:{hidden:false},'test-dialog':{},editor:{},'schedule-dialog':{},'sync-status':{}};
 const context={state:{section:'pipelines',project:'project'},document:{hidden:false,activeElement:{matches:()=>true},querySelector:()=>null},settingsActive:false,polling:false,$:id=>nodes[id],refreshCatalog:async value=>calls.push(value),uiText(){},uiAttribute(){},uiMessage:x=>x,uiFormatted(){},Date};
 const poll=runInNewContext('('+extract('async function pollCurrentPage(',"setInterval(()=>{if(!liveConnected")+')',context);
 await poll({eventDriven:true});assert.deepEqual(calls,[true]);
 await poll();assert.deepEqual(calls,[true,false]);
});

test('SSE hints refresh catalog state even when completion and log cursors are unchanged',async()=>{
 const source=new EventTarget();let refreshed=0;
 const context={state:{project:'p',run:null,section:'pipelines'},document:{hidden:false,querySelector:()=>null},liveKey:null,liveSource:null,liveConnected:false,lastLiveCompletion:null,sharedLiveSource:()=>source,api:async()=>({completion_cursor:0,log_cursor:0,run:null}),pollCurrentPage:async options=>{assert.equal(options.eventDriven,true);refreshed++;},query:()=>'',uiAttribute(){},$:()=>({}),MessageEvent};
 runInNewContext('('+extract('function syncLive()',"try{const connection=")+')',context)();
 for(let n=0;n<2;n++){source.dispatchEvent(new MessageEvent('change',{data:'{"hint":true}'}));await new Promise(setImmediate);}
 assert.equal(refreshed,2);
});

test('elapsed clocks advance without network activity and finished durations stay fixed',async()=>{
 const events=new EventTarget();globalThis.document=Object.assign(new EventTarget(),{hidden:false});globalThis.addEventListener=events.addEventListener.bind(events);
 try{
  const {elapsedText,elapsedNode,tickElapsed}=await import('../src/live-client.mjs');
  const node={dataset:{}};elapsedNode(node,1000,null);
  tickElapsed({querySelectorAll:()=>[node]},61000);assert.equal(node.textContent,'1 min 0 s elapsed · updating');
  assert.equal(elapsedText(1000,4600,90000),'3.6 s elapsed');
  elapsedNode(node,1000,4600);assert.equal(node.dataset.elapsedEnd,'4600');
  document.hidden=true;tickElapsed({querySelectorAll:()=>{throw Error('Hidden page must not tick');}},90000);
 }finally{delete globalThis.document;delete globalThis.addEventListener;}
});

test('queue state refreshes after a transition while its search control keeps focus',async()=>{
 const source=readFileSync(new URL('../src/executions.mjs',import.meta.url),'utf8');
 const body=source.slice(source.indexOf('let summaryLoading=false'),source.indexOf(' function connect()'));
 let state='running',loads=0;const context={document:{hidden:false},panel:{contains:()=>true},visible:true,since:0,signature:'',api:async()=>({state}),query:()=>'',load:()=>{loads++;},uiText(){},uiAttribute(){},counts:{},uiMessage:x=>x};
 context.renderSummary=data=>{context.signature=JSON.stringify(data);};
 const refresh=runInNewContext(body+';refreshSummary',context);
 await refresh();state='succeeded';await refresh();assert.equal(loads,2);
});

test('hour durations and compact stage clocks advance locally',async()=>{
 globalThis.document=Object.assign(new EventTarget(),{hidden:false});globalThis.addEventListener=()=>{};
 try{
  const {durationText,elapsedNode,tickElapsed}=await import('../src/live-client.mjs');
  assert.equal(durationText(3813000),'1 h 3 min 33 s');
  assert.equal(durationText(86400000),'24 h 0 min 0 s');
  const stage={dataset:{}};elapsedNode(stage,1000,null,true);
  tickElapsed({querySelectorAll:()=>[stage]},3814000);assert.equal(stage.textContent,'1 h 3 min 33 s');
  elapsedNode(stage,1000,4000,true);assert.equal(stage.textContent,'3.0 s');assert.equal(stage.dataset.elapsedEnd,'4000');
 }finally{delete globalThis.document;delete globalThis.addEventListener;}
});

test('completion notifications drain event pages without skipping events or duplicating selected-run notices',async()=>{
 const source=new EventTarget(),notices=[];let snapshot=0;
 const context={state:{project:'p',run:null,section:'pipelines'},document:{hidden:false,querySelector:()=>null},liveKey:null,liveSource:null,liveConnected:false,lastLiveCompletion:null,sharedLiveSource:()=>source,
  api:async url=>{if(url.startsWith('live/snapshot'))return {completion_cursor:snapshot++?105:0,log_cursor:0,run:null};const after=Number(new URLSearchParams(url.split('?')[1]).get('after')),end=Math.min(after+100,105);return {events:Array.from({length:end-after},(_,i)=>({run_id:'run-'+(after+i+1),status:'succeeded'})),next_cursor:end,has_more:end<105};},
  pollCurrentPage:async()=>{},query:value=>new URLSearchParams(value).toString(),uiAttribute(){},$:()=>({}),notice:text=>notices.push(text),uiError:x=>x,MessageEvent};
 runInNewContext('('+extract('function syncLive()',"try{const connection=")+')',context)();
 source.dispatchEvent(new MessageEvent('change',{data:'{"hint":true}'}));await new Promise(setImmediate);
 source.dispatchEvent(new MessageEvent('change',{data:'{"hint":true}'}));await new Promise(setImmediate);await new Promise(setImmediate);
 assert.equal(notices.length,105);assert.equal(new Set(notices).size,105);assert.equal(notices.at(-1),'Preflight: run run-105 completed — succeeded.');
});

test('workspace catalog reload preserves All workspaces with a project selected',async()=>{
 const option=(textContent,value)=>({textContent,value,dataset:{}});
 const listeners={};
 let refresh;
 const select={
  options:[option('All workspaces',''),option('Strox','strox')],
  value:'strox',dataset:{},isConnected:true,
  get selectedOptions(){return this.options.filter(option=>option.value===this.value);},
  replaceChildren(...options){this.options=options;},
  append(option){this.options.push(option);},
  addEventListener(name,handler){listeners[name]=handler;}
 };
 const remoteChoices=runInNewContext('('+extract('function remoteChoices(','const pipelineListName=')+')',{
  AbortController,state:{project:'strox'},query:()=>'',syncSelects(){},
  api:async()=>({items:[{id:'other',name:'Other'}],next_cursor:null}),
  cloneChoice:item=>item&&option(item.textContent,item.value),uiOption:option,
  setTimeout:callback=>{refresh=callback;},clearTimeout(){}
 });
 remoteChoices(select,'workspaces');
 listeners['choices-search']({detail:''});
 refresh();
 await new Promise(setImmediate);
 assert.deepEqual(select.options.map(option=>option.value),['','other','strox']);
 assert.equal(select.value,'strox');
});

test('any open dialog or custom selector defers page refresh without losing edits',async()=>{
 for(const selector of ['dialog[open]','.select-menu:popover-open','.row-menu[open]']){
  const context={state:{section:'pipelines',project:'p'},document:{hidden:false,querySelector:query=>query.includes(selector)?{}:null},settingsActive:false,polling:false,$:()=>({}),refreshCatalog:()=>{throw Error('Must not repaint an open control');}};
  await runInNewContext('('+extract('async function pollCurrentPage(',"setInterval(()=>{if(!liveConnected")+')',context)({eventDriven:true});
 }
});

test('unchanged history retains the current run selector',async()=>{
 const runs=[{run_id:'a',status:'running'}];let renders=0;
 const context={state:{pipeline:{pipeline_key:'key'},epoch:1,history:runs,historyNext:null},api:async()=>({runs,next_cursor:null}),query:()=>'',JSON,Set,$:()=>({}),renderHistory:()=>renders++};
 const load=runInNewContext('('+extract('async function loadHistory(','function renderHistory(')+')',context);
 await load();assert.equal(renders,0);
 runs[0]={run_id:'a',status:'succeeded'};context.state.history=[{run_id:'a',status:'running'}];
 await load();assert.equal(renders,1);
});

test('background view failures keep existing evidence and request a later retry',async()=>{
 const view={scrollTop:120,replaceChildren(){throw Error('Existing content was removed');}},state={epoch:1,tab:'Tests'};let error;
 const context={state,$:id=>id==='view'?view:{},el:()=>({}),renderViewContent:async()=>{throw Error('Connection lost');},uiAttribute:(_node,_key,value)=>error=value};
 const render=runInNewContext('('+extract('async function renderView(','async function renderViewContent(')+')',context);
 await render({background:true});assert.equal(state.viewDirty,true);assert.equal(view.scrollTop,120);assert.equal(error,'Connection lost');
});

test('discard confirmation is nonblocking and keeps edits unless explicitly discarded',()=>{
 const closed=[],editor={},nodes=[];let dirty=true;
 const el=(tag,text)=>({tag,text,children:[],classList:{add(){}},setAttribute(){},append(...children){this.children.push(...children);},addEventListener(){},showModal(){this.open=true;},focus(){this.focused=true;}});
 const context={state:{editorKind:'pipeline'},editorDirty:()=>dirty,$:()=>editor,closeDialog:node=>closed.push(node),document:{getElementById:()=>null,body:{append:node=>nodes.push(node)}},el,button:(text,click)=>Object.assign(el('button',text),{click})};
 const close=runInNewContext('('+extract('function requestEditorClose(','function setEditorMode(')+')',context);
 close();assert.equal(closed.length,0);const dialog=nodes[0],[keep,discard]=dialog.children[1].children;
 assert.equal(dialog.open,true);assert.equal(keep.focused,true);keep.click();assert.deepEqual(closed,[dialog]);discard.click();assert.equal(closed.at(-1),editor);
 dirty=false;close();assert.equal(closed.at(-1),editor);
});

test('clearing a pipeline closes its dialog without hiding an open modal',()=>{
 const editor={open:true,hidden:false},nodes={editor,history:{replaceChildren(){}},edit:{},start:{},'more-history':{}};let requested=false;
 const context={state:{},$:id=>nodes[id],clearRun(){},closeDialog:dialog=>{assert.equal(dialog,editor);requested=true;return new Promise(()=>{});}};
 runInNewContext('('+extract('function clearPipeline(','async function loadProjects(')+')',context)();
 assert.equal(requested,true);assert.equal(editor.hidden,false);
});
