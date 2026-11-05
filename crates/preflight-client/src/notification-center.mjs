import {mountVirtualList} from './virtual-list.mjs';
const KEY='preflight.notifications.v1';
export function notificationJournal(storage,{now=Date.now,id=()=>crypto.randomUUID()}={}){
 let items=[];
 function reload(){try{const value=JSON.parse(storage?.getItem(KEY)||'[]');items=Array.isArray(value)?value.filter(x=>x&&typeof x.id==='string'&&typeof x.message==='string'&&['info','error'].includes(x.kind)&&Number.isFinite(x.at)).map(x=>({...x,message:x.message.slice(0,4000),count:Math.max(1,Number(x.count)||1),read:x.read===true})):[];}catch{items=[];}}
 function save(){try{storage?.setItem(KEY,JSON.stringify(items));}catch{/* Keep this session usable when browser storage is unavailable. */}}
 reload();
 return {get items(){return items;},reload,add(message,error=false){
  if(!error&&/^(Copied\.?|Link copied\.?)$/i.test(message.trim()))return;
  const kind=error?'error':'info',last=items[0];
  if(last?.message===message&&last.kind===kind){last.count++;last.at=now();last.read=false;}
  else items.unshift({id:id(),message:message.slice(0,4000),kind,at:now(),count:1,read:false});
  save();
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
 function pagedList(){const list=el('div',undefined,'notification-list');list.tabIndex=0;list.setAttribute('role','region');list.setAttribute('aria-label','Notification list');return list;}
 function row(item){const box=el('article',undefined,'notification-entry '+item.kind);box.classList.toggle('unread',!item.read);box.append(el('span',item.kind==='error'?'!':'✓','notification-type'));const body=el('div');body.append(el('p',item.message),el('time',new Date(item.at).toLocaleString('en-GB',{hour12:false})+(item.count>1?' · ×'+item.count:''),'muted'));box.append(body);return box;}
 function heading(title,close){const top=el('div',undefined,'heading');top.append(el('h2',title),modalClose(button('Close',close)));return top;}
 const list=pagedList(),full=pagedList(),actions=el('div',undefined,'actions'),filters=el('div',undefined,'actions');
 const read=button('Mark all as read',()=>{journal.markRead();render();}),mark=button('Mark all as read',()=>{journal.markRead();render();});
 actions.append(read,button('View all',()=>{panel.hidePopover();render();dialog.showModal();}));
 panel.append(heading('Notifications',()=>panel.hidePopover()),actions,list);
 for(const value of ['all','unread','errors']){const b=button(value[0].toUpperCase()+value.slice(1),()=>{filter=value;full.scrollTop=0;render();});b.dataset.filter=value;filters.append(b);}
 dialog.append(heading('Notification history',()=>closeDialog(dialog)),el('p','Notification history in this browser.','muted'),filters,mark,full);
 const options={render:row,key:item=>item.id,version:JSON.stringify,estimate:80,windowSize:60};
 const panelList=mountVirtualList(list,options),historyList=mountVirtualList(full,options);
 const panelEmpty=el('p','No notifications.','empty-state'),historyEmpty=el('p','No notifications.','empty-state');list.append(panelEmpty);full.append(historyEmpty);
 function render(){const unread=journal.items.filter(x=>!x.read).length;count.textContent=String(unread);count.hidden=!unread;bell.setAttribute('aria-label',unread?`Notifications · ${unread} unread`:'Notifications');read.disabled=mark.disabled=!unread;
  for(const b of filters.children)b.setAttribute('aria-pressed',String(b.dataset.filter===filter));
  const selected=journal.items.filter(x=>filter==='all'||(filter==='unread'?!x.read:x.kind==='error'));panelList.setItems(journal.items);historyList.setItems(selected);panelEmpty.hidden=journal.items.length>0;historyEmpty.hidden=selected.length>0;
 }
 panel.addEventListener('toggle',event=>bell.setAttribute('aria-expanded',String(event.newState==='open')));
 addEventListener('storage',event=>{if(event.key===KEY){journal.reload();render();}});
 render();return {add(message,error){journal.add(message,error);render();}};
}
