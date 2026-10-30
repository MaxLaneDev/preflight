// One connection per browser origin. Messages contain invalidation hints, not run data.
const ports=new Map();let source=null,retry=null,seen=false;
function send(port,message){try{port.postMessage(message);}catch{ports.delete(port);}}
function broadcast(type){for(const [port,state] of ports)if(state.active)send(port,{type});}
function reconcile(){
 const active=[...ports.values()].some(state=>state.active);
 if(!active){source?.close();source=null;seen=false;if(retry)clearTimeout(retry);retry=null;return;}
 if(source||retry)return;
 const stream=new EventSource('/api/v1/live?hints=true');source=stream;
 stream.onopen=()=>{if(source===stream)broadcast('open');};
 stream.addEventListener('change',()=>{if(source===stream){seen=true;broadcast('change');}});
 stream.onerror=()=>{if(source!==stream)return;broadcast('error');if(stream.readyState===2){stream.close();source=null;retry=setTimeout(()=>{retry=null;reconcile();},5000);}};
}
self.onconnect=event=>{
 const port=event.ports[0];
 if(ports.size>=128){send(port,{type:'error'});port.close();return;}
 ports.set(port,{active:false,last:Date.now()});
 port.onmessage=event=>{const state=ports.get(port);if(!state)return;state.last=Date.now();
  if(event.data?.type==='ping'){send(port,{type:'pong'});return;}
  if(event.data?.type==='disconnect'){ports.delete(port);port.close();reconcile();return;}
  if(event.data?.type==='active'){state.active=true;reconcile();if(source?.readyState===1)send(port,{type:'open'});if(seen)send(port,{type:'change'});}
 };
 port.onmessageerror=()=>{ports.delete(port);port.close();reconcile();};port.start();
};
setInterval(()=>{const now=Date.now();for(const [port,state] of ports)if(now-state.last>45000){ports.delete(port);port.close();}reconcile();},15000);
