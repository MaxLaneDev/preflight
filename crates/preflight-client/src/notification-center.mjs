const KEY='preflight.notifications.v1',LIMIT=200;
export function notificationJournal(storage,{now=Date.now,id=()=>crypto.randomUUID()}={}){
 let items=[];
 function reload(){try{const value=JSON.parse(storage?.getItem(KEY)||'[]');items=Array.isArray(value)?value.filter(x=>x&&typeof x.id==='string'&&typeof x.message==='string'&&['info','error'].includes(x.kind)&&Number.isFinite(x.at)).slice(0,LIMIT).map(x=>({...x,message:x.message.slice(0,4000),count:Math.max(1,Number(x.count)||1),read:x.read===true})):[];}catch{items=[];}}
 function save(){try{storage?.setItem(KEY,JSON.stringify(items));}catch{/* Keep this session usable when browser storage is unavailable. */}}
 reload();
 return {get items(){return items;},reload,add(message,error=false){
  if(!error&&/^(Copied\.?|Link copied\.?)$/i.test(message.trim()))return;
  const kind=error?'error':'info',last=items[0];
  if(last?.message===message&&last.kind===kind){last.count++;last.at=now();last.read=false;}
  else items.unshift({id:id(),message:message.slice(0,4000),kind,at:now(),count:1,read:false});
  items=items.slice(0,LIMIT);save();
 },markRead(){for(const item of items)item.read=true;save();}};
}
export function installNotificationCenter({el,button,modalClose,closeDialog}){
 let storage;try{storage=localStorage;}catch{/* Private browser modes may disable persistence. */}
 const journal=notificationJournal(storage),bell=button('',()=>{panel.matches(':popover-open')?panel.hidePopover():panel.showPopover();});
 bell.id='notifications-toggle';bell.className='notification-bell';bell.title='Notifications';bell.setAttribute('aria-label','Notifications');bell.setAttribute('aria-haspopup','dialog');bell.setAttribute('aria-expanded','false');
 bell.innerHTML='<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9M10 21h4"/></svg>';
 const count=el('span','','notification-count');bell.append(count);document.querySelector('footer').append(bell);
 const panel=el('section',undefined,'notification-panel');panel.popover='auto';panel.setAttribute('role','dialog');panel.setAttribute('aria-label','Notifications');panel.id='notification-panel';bell.setAttribute('aria-controls',panel.id);
 const dialog=el('dialog',undefined,'notification-history');dialog.setAttribute('aria-label','Notification history');document.body.append(panel,dialog);
 let filter='all';
 function row(item){const box=el('article',undefined,'notification-entry '+item.kind);box.classList.toggle('unread',!item.read);box.append(el('span',item.kind==='error'?'!':'✓','notification-type'));const body=el('div');body.append(el('p',item.message),el('time',new Date(item.at).toLocaleString('en-GB',{hour12:false})+(item.count>1?' · ×'+item.count:''),'muted'));box.append(body);return box;}
 function heading(title,close){const top=el('div',undefined,'heading');top.append(el('h2',title),modalClose(button('Close',close)));return top;}
 function render(){const unread=journal.items.filter(x=>!x.read).length;count.textContent=String(unread);count.hidden=!unread;bell.setAttribute('aria-label',unread?`Notifications · ${unread} unread`:'Notifications');
  const actions=el('div',undefined,'actions'),read=button('Mark all as read',()=>{journal.markRead();render();});read.disabled=!unread;actions.append(read,button('View all',()=>{panel.hidePopover();render();dialog.showModal();}));
  const list=el('div',undefined,'notification-list');list.append(...journal.items.slice(0,8).map(row));if(!journal.items.length)list.append(el('p','No notifications.','empty-state'));panel.replaceChildren(heading('Notifications',()=>panel.hidePopover()),actions,list);
  const filters=el('div',undefined,'actions');for(const value of ['all','unread','errors']){const b=button(value[0].toUpperCase()+value.slice(1),()=>{filter=value;render();});b.setAttribute('aria-pressed',String(value===filter));filters.append(b);}
  const full=el('div',undefined,'notification-list'),selected=journal.items.filter(x=>filter==='all'||(filter==='unread'?!x.read:x.kind==='error'));full.append(...selected.map(row));if(!selected.length)full.append(el('p','No notifications.','empty-state'));
  const mark=button('Mark all as read',()=>{journal.markRead();render();});mark.disabled=!unread;dialog.replaceChildren(heading('Notification history',()=>closeDialog(dialog)),el('p','Latest 200 notifications in this browser.','muted'),filters,mark,full);
 }
 panel.addEventListener('toggle',event=>bell.setAttribute('aria-expanded',String(event.newState==='open')));
 addEventListener('storage',event=>{if(event.key===KEY){journal.reload();render();}});
 render();return {add(message,error){journal.add(message,error);render();}};
}
