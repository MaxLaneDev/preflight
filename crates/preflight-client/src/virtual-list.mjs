// Variable-height rows, bounded DOM, and a stable visible anchor when pages arrive.
export function mountVirtualList(view,{render,key,estimate=32,windowSize=100,host=view,tag='div'}){
 let items=[],heights=new Map(),prefix=[0],frame=0,drawing=false,destroyed=false;
 const doc=view.ownerDocument,top=doc.createElement(tag),body=doc.createElement(tag),bottom=doc.createElement(tag);if(tag==='tbody')for(const spacer of [top,bottom]){spacer.innerHTML='<tr><td colspan=4 style="padding:0;border:0;height:inherit"></td></tr>';spacer.className='virtual-table-spacer';}
 top.setAttribute('aria-hidden','true');bottom.setAttribute('aria-hidden','true');host.append(top,body,bottom);view.style.overflowAnchor='none';
 const offsets=()=>{prefix=[0];for(const item of items)prefix.push(prefix.at(-1)+(heights.get(key(item))||estimate));};
 const indexAt=y=>{let lo=0,hi=items.length;while(lo<hi){const mid=(lo+hi)>>1;if(prefix[mid+1]<=y)lo=mid+1;else hi=mid;}return Math.min(lo,Math.max(0,items.length-1));};
 function draw(end=false,position=view.scrollTop){
  if(destroyed||drawing)return;drawing=true;
  const anchor=indexAt(position),delta=position-prefix[anchor];
  const start=end?Math.max(0,items.length-windowSize):Math.max(0,Math.min(anchor-10,items.length-windowSize));
  const stop=Math.min(items.length,start+windowSize),old=new Map([...body.children].map(n=>[n.dataset.virtualKey,n]));
  const rows=items.slice(start,stop).map(item=>{const k=String(key(item)),row=old.get(k)||render(item);row.dataset.virtualKey=k;return row;});
  if(rows.length!==body.children.length||rows.some((row,index)=>row!==body.children[index]))body.replaceChildren(...rows);top.style.height=prefix[start]+'px';bottom.style.height=(prefix.at(-1)-prefix[stop])+'px';
  for(let n=0;n<rows.length;n++)heights.set(key(items[start+n]),Math.max(1,rows[n].getBoundingClientRect().height));
  offsets();top.style.height=prefix[start]+'px';bottom.style.height=(prefix.at(-1)-prefix[stop])+'px';
  view.scrollTop=end?view.scrollHeight:prefix[anchor]+delta;drawing=false;
 }
 const onScroll=()=>{if(!frame&&!destroyed)frame=requestAnimationFrame(()=>{frame=0;draw();});};view.addEventListener('scroll',onScroll,{passive:true});
 const observer=new ResizeObserver(()=>{if(view.isConnected)draw(view.scrollHeight-view.clientHeight-view.scrollTop<2);});observer.observe(view);observer.observe(body);
 return {
  setItems(next,{end=false,redraw=false}={}){
   const index=indexAt(view.scrollTop),anchor=items[index]&&key(items[index]),delta=view.scrollTop-prefix[index];items=next.slice();
   const keys=new Set(items.map(key));heights=new Map([...heights].filter(([k])=>keys.has(k)));offsets();
   if(redraw)body.replaceChildren();
   const found=items.findIndex(item=>key(item)===anchor);
   draw(end,found>=0?prefix[found]+delta:view.scrollTop);
  },
  end(){draw(true);},
  get rendered(){return body.children.length;},
  destroy(){destroyed=true;observer.disconnect();view.removeEventListener('scroll',onScroll);if(frame)cancelAnimationFrame(frame);}
 };
}

// One cursor request at a time; short pages fill the viewport without a click.
export function mountInfiniteScroll(view,load,{host=view}={}){
 const status=view.ownerDocument.createElement('div'),win=view.ownerDocument.defaultView;
 status.className='infinite-scroll-status';status.setAttribute('role','status');host.append(status);
 let more=false,busy=false,error=false,disposed=false,frame=0,retryAfter=0;
 async function next(){if(disposed||busy||error||!more||!status.isConnected||!view.getClientRects().length)return;busy=true;paint();try{await load();}catch{error=true;retryAfter=Date.now()+1000;}finally{busy=false;paint();check();}}
 function paint(){status.hidden=!more&&!busy&&!error;status.setAttribute('aria-busy',String(busy));status.textContent=busy?'Loading…':error?'Unable to load more. Scroll to retry.':'';}
 function check(){if(frame||disposed)return;frame=requestAnimationFrame(()=>{frame=0;if(disposed||!status.isConnected)return;const a=status.getBoundingClientRect(),b=view.getBoundingClientRect();if(a.top<=b.bottom+160&&a.bottom>=b.top)next();});}
 function resume(){if(error&&Date.now()>=retryAfter){error=false;more=true;paint();}check();}
 view.addEventListener('scroll',resume,{passive:true});view.addEventListener('wheel',resume,{passive:true});win.addEventListener('online',resume);const observer=new ResizeObserver(check);observer.observe(view);
 return {check,get connected(){return status.isConnected;},set(state){more=Boolean(state.more);error=Boolean(state.error);if(error)retryAfter=Date.now()+1000;paint();check();},destroy(){disposed=true;observer.disconnect();view.removeEventListener('scroll',resume);view.removeEventListener('wheel',resume);win.removeEventListener('online',resume);if(frame)cancelAnimationFrame(frame);status.remove();}};
}

// Keep card grids responsive while windowing complete rows rather than individual columns.
export function mountVirtualCards(view,{host=view,render,key,minWidth=280,gap=18,estimate=240}){
 let items=[],columns=0;const list=mountVirtualList(view,{host,key:row=>key(row[0]),estimate,windowSize:40,render:row=>{const group=view.ownerDocument.createElement('div');group.className='virtual-card-row';group.style.gridTemplateColumns=`repeat(${columns},minmax(0,1fr))`;group.style.gap=gap+'px';group.append(...row.map(render));return group;}});
 function update(){const next=Math.max(1,Math.floor((host.clientWidth+gap)/(minWidth+gap))),changed=next!==columns;columns=next;const rows=[];for(let i=0;i<items.length;i+=columns)rows.push(items.slice(i,i+columns));list.setItems(rows,{redraw:changed});}
 const observer=new ResizeObserver(()=>{if(host.isConnected)update();});observer.observe(host);
 return {setItems(next){items=next.slice();update();},destroy(){observer.disconnect();list.destroy();}};
}

// Legacy cursor controls keep their state; the visible control is a scroll sentinel.
export const isPageLabel=text=>/^(?:More (?:matches|tests|result changes|changed files|text|test choices|diagnostics|provider runs|artifacts)|Next preview page|Review next batch|Older (?:runs|occurrences|revisions)|Load (?:more(?: runs| archives)?|older runs))(?:$| ·)/.test(String(text));
const pageControls=new Map();let pageObserver;
function scrollParent(node){for(let parent=node.parentElement;parent;parent=parent.parentElement)if(/auto|scroll/.test(getComputedStyle(parent).overflowY))return parent;return node.closest('dialog')||document.querySelector('main')||document.documentElement;}
function syncPages(records=[]){const layoutChanged=records.some(record=>record.type==='attributes'&&record.attributeName==='hidden');for(const [node,control]of pageControls){
 if(!node.isConnected){if(control.connected){control.pager?.destroy();pageControls.delete(node);}continue;}control.connected=true;if(control.pager&&!control.pager.connected){control.pager.destroy();control.pager=null;control.signature=null;}
 const signature=[node.hidden,node.disabled,node.getAttribute('aria-busy'),node.dataset.pageError,Boolean(control.active),control.choicesOnly].join('|');if(signature===control.signature){if(layoutChanged)control.pager?.check();continue;}control.signature=signature;
 for(const select of control.choices){select.dataset.more=String(!node.hidden);select.dataset.loading=String(node.disabled||Boolean(control.active));select.dataset.error=node.dataset.pageError||'';}
 if(control.choicesOnly){control.pager?.destroy();control.pager=null;continue;}
 if(!control.pager){control.pager=mountInfiniteScroll(scrollParent(node),control.invoke,{host:node.parentElement});}
 control.pager.set({more:!node.hidden&&!node.disabled&&node.getAttribute('aria-busy')!=='true',error:node.dataset.pageError});
}}
export function registerPageButton(node,load){
 if(pageControls.has(node))return pageControls.get(node).invoke;
 node.classList.add('auto-page-trigger');node.setAttribute('aria-hidden','true');node.tabIndex=-1;
 const control={load,choices:new Set(),connected:false,signature:null,active:null};
 control.invoke=()=>{if(control.active)return control.active;node.dataset.pageError='';control.active=Promise.resolve().then(()=>control.load()).catch(error=>{node.dataset.pageError=error.message||'Unable to load this page';throw error;}).finally(()=>{control.active=null;control.signature=null;syncPages();});return control.active;};
 pageControls.set(node,control);
 if(!pageObserver){pageObserver=new MutationObserver(syncPages);pageObserver.observe(node.ownerDocument.body,{subtree:true,childList:true,attributes:true,attributeFilter:['hidden','disabled','aria-busy','data-page-error']});}
 queueMicrotask(syncPages);return control.invoke;
}
export function bindPageChoices(node,selects,{choicesOnly=true}={}){
 const control=pageControls.get(node);if(!control)throw Error('Unregistered cursor loader');control.choicesOnly=choicesOnly;
 for(const select of selects){control.choices.add(select);select.addEventListener('choices-more',event=>event.detail?.waitUntil?.(control.invoke()));}
 control.signature=null;queueMicrotask(syncPages);
}
