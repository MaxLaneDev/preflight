// Without SharedWorker support use the existing bounded HTTP polling, never per-tab SSE.
let enabled=false,worker=null,heartbeat=null,lastPong=0;const sources=new Set();
function emit(type){for(const source of [...sources]){const event=type==='change'?new MessageEvent(type,{data:'{"hint":true}'}):new Event(type);source.dispatchEvent(event);source['on'+type]?.(event);}}
function disconnect(){if(heartbeat)clearInterval(heartbeat);heartbeat=null;if(worker){worker.port.postMessage({type:'disconnect'});worker.port.close();worker=null;}}
function connect(){
 if(!enabled||worker||!sources.size||document.hidden||typeof SharedWorker==='undefined')return;
 try{const current=new SharedWorker('/live-worker.js',{name:'preflight-live-v1'});worker=current;
  current.port.onmessage=event=>{if(worker!==current)return;lastPong=Date.now();if(['open','change','error'].includes(event.data?.type))emit(event.data.type);};
  current.onerror=()=>{if(worker===current){disconnect();emit('error');}};
  current.port.start();lastPong=Date.now();current.port.postMessage({type:'active'});heartbeat=setInterval(()=>{if(Date.now()-lastPong>30000){disconnect();emit('error');connect();}else current.port.postMessage({type:'ping'});},10000);
 }catch{disconnect();emit('error');}
}
export function configureLive(value){enabled=value===true;if(!enabled)disconnect();else connect();}
export function liveSource(){const source=new EventTarget();source.close=()=>{sources.delete(source);if(!sources.size)disconnect();};sources.add(source);connect();return source;}
addEventListener('pagehide',disconnect);addEventListener('pageshow',connect);document.addEventListener('visibilitychange',()=>document.hidden?disconnect():connect());

// Elapsed time advances locally even when a quiet build emits no SSE events.
export function elapsedText(start,end,now=Date.now()){
 const ms=Math.max(0,(end??now)-start);
 const value=ms<1000?Math.round(ms)+' ms':ms<60000?(ms/1000).toFixed(1)+' s':Math.floor(ms/60000)+' min '+Math.floor(ms%60000/1000)+' s';
 return value+' elapsed'+(end==null?' · updating':'');
}
export function elapsedNode(node,start,end){
 node.dataset.elapsedStart=String(start);
 if(end==null)delete node.dataset.elapsedEnd;else node.dataset.elapsedEnd=String(end);
 node.textContent=elapsedText(start,end);return node;
}
export function tickElapsed(root=document,now=Date.now()){
 if(document.hidden)return;
 for(const node of root.querySelectorAll('[data-elapsed-start]:not([data-elapsed-end])'))node.textContent=elapsedText(Number(node.dataset.elapsedStart),null,now);
}
