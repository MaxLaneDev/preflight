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
