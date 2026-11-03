import {msg,attr} from './localization.mjs';
const closing=new WeakMap(),effects=new WeakMap(),icons=new WeakSet(),pages=new WeakMap();
const reducedMotion=()=>globalThis.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
function animate(node,frames,duration){
 effects.get(node)?.cancel();
 if(reducedMotion()||!node.animate)return null;
 const effect=node.animate(frames,{duration,easing:'ease-out'});effects.set(node,effect);return effect;
}
export function modalClose(button){
 button.classList.add('dialog-close');button.type='button';
 if(icons.has(button))return button;
 icons.add(button);
 if(!button.hasAttribute('aria-label'))attr(button,'aria-label',msg('Close'));
 attr(button,'title',msg('Close'));
 button.innerHTML='<svg aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="m6 6 12 12M18 6 6 18"/></svg>';
 return button;
}
export function closeDialog(dialog,value){
 if(closing.has(dialog))return closing.get(dialog);
 if(!dialog.open)return Promise.resolve();
 const effect=animate(dialog,[{opacity:1,transform:'translateY(0) scale(1)'},{opacity:0,transform:'translateY(4px) scale(.99)'}],100);
 const done=(async()=>{try{if(effect)await effect.finished;}catch{/* A cancelled animation must still close the dialog. */}finally{if(dialog.open)dialog.close(value);closing.delete(dialog);}})();
 if(effect)closing.set(dialog,done);
 return done;
}
export function animatePage(node,key){
 if(pages.get(node)===key)return;
 pages.set(node,key);
 animate(node,[{opacity:.6,transform:'translateY(3px)'},{opacity:1,transform:'translateY(0)'}],120);
}
export function elasticDrag(node,scale,redraw){
 let drag=null,suppressClick=false;
 node.addEventListener('pointerdown',event=>{if(event.button!==0||event.isPrimary===false)return;drag={id:event.pointerId,x:event.clientX,y:event.clientY,moved:false};suppressClick=false;});
 node.addEventListener('pointermove',event=>{
  if(!drag||drag.id!==event.pointerId)return;
  const x=event.clientX-drag.x,y=event.clientY-drag.y;
  if(!drag.moved&&Math.hypot(x,y)<4)return;
  drag.moved=true;node.setPointerCapture(event.pointerId);event.preventDefault();
  node.style.transform=`translate(${x/scale()}px,${y/scale()}px)`;redraw();
 });
 const release=()=>{if(!drag)return;suppressClick=drag.moved;drag=null;node.style.transform='';redraw();};
 for(const name of ['pointerup','pointercancel','lostpointercapture'])node.addEventListener(name,release);
 node.addEventListener('click',event=>{if(suppressClick){event.preventDefault();event.stopImmediatePropagation();suppressClick=false;}},true);
}
export function installDialogInteractions(root=document){
 const enhanced=new WeakSet();
 function enhance(dialog){
  if(!enhanced.has(dialog)){enhanced.add(dialog);if(!dialog.hasAttribute('data-close-guard'))dialog.addEventListener('cancel',event=>{if(!event.defaultPrevented){event.preventDefault();closeDialog(dialog);}});}
  for(const button of dialog.querySelectorAll('.dialog-close')){
   modalClose(button);
   const title=dialog.querySelector('h1,h2');if(!title)continue;
   let heading=title.closest('.heading');
   if(!heading){heading=document.createElement('div');heading.className='heading modal-heading';title.before(heading);heading.append(title);}
   if(button.parentElement!==heading)heading.append(button);
  }
 }
 // Guarded editors own Escape, including their unsaved-draft confirmation.
 root.querySelectorAll('dialog').forEach(enhance);
 new MutationObserver(records=>{for(const record of records){
  if(record.type==='attributes'&&record.target.tagName==='DIALOG'){
   const dialog=record.target;enhance(dialog);
   if(dialog.open&&!closing.has(dialog))animate(dialog,[{opacity:0,transform:'translateY(6px) scale(.99)'},{opacity:1,transform:'translateY(0) scale(1)'}],120);
  }else if(record.type==='childList')for(const node of record.addedNodes)if(node.nodeType===1){
   if(node.matches('dialog'))enhance(node);else{const dialog=node.closest('dialog');if(dialog)enhance(dialog);node.querySelectorAll('dialog').forEach(enhance);}
  }
 }}).observe(root.body,{subtree:true,childList:true,attributes:true,attributeFilter:['open']});
}
