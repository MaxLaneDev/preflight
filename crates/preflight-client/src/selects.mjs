import {registerPageButton} from './virtual-list.mjs';
import {msg as uiMessage,put as uiText,attr as uiAttribute,uiNode,uiError} from '/localization.mjs';
// Native selects retain form values; one accessible popover supplies their visible UI.
export function installSelects(root=document){
 const controls=new Map();let serial=0;
 function enhance(select){
  if(controls.has(select)||select.multiple||select.size>1||select.classList.contains('sr-only'))return;
  const key=select.id||'choice-'+(++serial),wrapper=document.createElement('span'),trigger=document.createElement('button'),menu=document.createElement('div'),search=document.createElement('input'),list=document.createElement('div'),more=document.createElement('button'),status=document.createElement('div');
  wrapper.className='select-control';trigger.type='button';trigger.id=key+'-control';trigger.className='select-trigger';trigger.setAttribute('role','combobox');trigger.setAttribute('aria-haspopup','listbox');trigger.setAttribute('aria-expanded','false');
  menu.className='select-menu';menu.popover='auto';list.id=key+'-options';list.setAttribute('role','listbox');list.tabIndex=0;trigger.setAttribute('aria-controls',list.id);
  search.type='search';uiAttribute(search,"placeholder",uiMessage("Search…"));uiAttribute(search,'aria-label',uiMessage("Filter options"));search.setAttribute('aria-controls',list.id);more.type='button';uiText(more,uiMessage("Load more"));more.className='select-more';status.className='select-status';status.setAttribute('role','status');menu.append(search,list,status,more);more.onclick=registerPageButton(more,async()=>{let pending;select.dispatchEvent(new CustomEvent('choices-more',{detail:{waitUntil:value=>pending=value}}));await pending;if(select.dataset.error)throw Error(select.dataset.error);});const valueLabel=document.createElement('span');valueLabel.className='select-value';trigger.append(valueLabel);wrapper.append(trigger);select.after(wrapper);select.classList.add('native-select');select.tabIndex=-1;select.setAttribute('aria-hidden','true');
  let active=-1,visible=[],menuSignature="";
  const signature=()=>JSON.stringify([...select.options].map(o=>[o.value,o.textContent,o.selected,o.disabled,o.hidden,o.dataset.search]));
  function name(){return select.getAttribute('aria-label')||[...(select.labels||[])].map(label=>label.childNodes[0]?.textContent||'').join(' ').trim()||uiMessage('Choose an option');}
  function sync(){
   wrapper.hidden=select.hidden;trigger.disabled=select.disabled;if((select.hidden||select.disabled)&&menu.matches(':popover-open'))close();for(const attr of ['aria-describedby','aria-invalid','aria-required']){const value=select.getAttribute(attr);if(value===null)trigger.removeAttribute(attr);else trigger.setAttribute(attr,value);}trigger.setAttribute('aria-required',String(select.required));const label=select.selectedOptions[0]?.textContent||uiMessage('Choose…');if(valueLabel.textContent!==label)uiText(valueLabel,label);uiAttribute(trigger,"title",label);uiAttribute(trigger,'aria-label',name());uiAttribute(list,'aria-label',uiMessage("{0} options",{"0":name()}));
   more.hidden=select.dataset.more!=='true'&&!select.dataset.error;more.disabled=select.dataset.loading==='true';more.dataset.pageError=select.dataset.error||'';uiText(more,(select.dataset.error?uiMessage("Retry"):uiMessage("Load more")));uiText(status,(select.dataset.loading==='true'?uiMessage("Loading…"):(select.dataset.error?uiMessage("Unable to load options."):'')));trigger.setAttribute('aria-busy',String(select.dataset.loading==='true'));if(menu.matches(':popover-open')){search.hidden=select.dataset.remote!=='true'&&select.options.length<=8;if(signature()!==menuSignature)render();position();}
  }
  function position(){
   if(!trigger.isConnected){menu.hidePopover();return;}
   const r=trigger.getBoundingClientRect(),gap=6,view=window.visualViewport,top=view?.offsetTop||0,bottom=top+(view?.height||innerHeight),availableBelow=bottom-r.bottom-8,availableAbove=r.top-top-8,above=availableBelow<180&&availableAbove>availableBelow;
   menu.style.width=Math.min(r.width,innerWidth-16)+'px';menu.style.maxHeight=Math.max(80,Math.min(360,above?availableAbove-gap:availableBelow-gap))+'px';
   menu.style.left=Math.max(8,Math.min(r.left,innerWidth-menu.getBoundingClientRect().width-8))+'px';menu.style.top=above?Math.max(top+8,r.top-menu.getBoundingClientRect().height-gap)+'px':r.bottom+gap+'px';
  }
  function highlight(index,reveal=true){
   active=Math.max(0,Math.min(visible.length-1,index));for(let i=0;i<visible.length;i++)visible[i].element.classList.toggle('highlighted',i===active);
   const option=visible[active];if(option){search.setAttribute('aria-activedescendant',option.element.id);list.setAttribute('aria-activedescendant',option.element.id);if(reveal)option.element.scrollIntoView({block:'nearest'});}else{search.removeAttribute('aria-activedescendant');list.removeAttribute('aria-activedescendant');}
  }
  function choose(option){if(select.disabled||option.disabled)return;select.value=option.value;close(true);select.dispatchEvent(new Event('input',{bubbles:true}));select.dispatchEvent(new Event('change',{bubbles:true}));sync();}
  function render(keepScroll=true){
   const savedScroll=list.scrollTop,previous=visible[active]?.option.value;menuSignature=signature();list.replaceChildren();visible=[];const q=search.value.trim().toLocaleLowerCase();
   for(const [index,option]of [...select.options].entries()){
    if(option.hidden||!`${option.textContent} ${option.value} ${option.dataset.search||''}`.toLocaleLowerCase().includes(q))continue;
    const row=document.createElement('div');row.id=key+'-option-'+index;row.className='select-option';row.setAttribute('role','option');row.setAttribute('aria-selected',String(option.selected));row.setAttribute('aria-disabled',String(option.disabled||select.disabled));uiText(row,option.textContent);if(option.selected){const mark=document.createElement('span');mark.className='selected-mark';mark.setAttribute('aria-hidden','true');uiText(mark,'✓');row.append(mark);}row.dataset.value=option.value;
    row.onclick=()=>choose(option);list.append(row);if(!option.disabled)visible.push({option,element:row});
   }
   if(!list.childElementCount&&select.dataset.loading!=='true'){const empty=document.createElement('p');empty.className='muted';uiText(empty,uiMessage("No matches"));list.append(empty);}
   list.append(more);active=visible.findIndex(row=>row.option.value===previous);if(active<0)active=visible.findIndex(row=>row.option.selected);highlight(active<0?0:active,!keepScroll);if(keepScroll)list.scrollTop=savedScroll;
  }
  function open(last=false){
   if(trigger.disabled)return;const parent=select.closest('dialog[open]')||document.body;if(menu.parentElement!==parent)parent.append(menu);active=-1;visible=[];search.value='';if(select.dataset.remote==='true')select.dispatchEvent(new CustomEvent('choices-search',{detail:''}));menu.showPopover();trigger.setAttribute('aria-expanded','true');search.hidden=select.dataset.remote!=='true'&&select.options.length<=8;render(false);if(last)highlight(visible.length-1);position();(search.hidden?list:search).focus();
  }
  function close(focus=false){menu.hidePopover();trigger.setAttribute('aria-expanded','false');if(focus&&trigger.isConnected)trigger.focus();}
  trigger.onclick=()=>menu.matches(':popover-open')?close():open();
  trigger.onkeydown=event=>{if(['ArrowDown','ArrowUp','Home','End'].includes(event.key)){event.preventDefault();open(event.key==='End'||event.key==='ArrowUp');if(event.key==='Home')highlight(0);}};
  search.oninput=()=>{render(false);position();if(select.dataset.remote==='true')select.dispatchEvent(new CustomEvent('choices-search',{detail:search.value}));};
  search.onkeydown=event=>{
   if(event.key==='Escape'){event.preventDefault();event.stopPropagation();close(true);}
   else if(event.key==='Tab'){close(true);}
   else if(['ArrowDown','ArrowUp','Home','End'].includes(event.key)){event.preventDefault();highlight(event.key==='Home'?0:event.key==='End'?visible.length-1:active+(event.key==='ArrowDown'?1:-1));}
   else if(event.key==='Enter'){event.preventDefault();if(visible[active])choose(visible[active].option);}
  };
  list.onkeydown=search.onkeydown;
  menu.addEventListener('toggle',event=>{trigger.setAttribute('aria-expanded',String(event.newState==='open'));});
  select.addEventListener('change',sync);
  for(const label of select.labels||[]){label.addEventListener('click',event=>{if(event.target!==label)return;event.preventDefault();trigger.focus();});}
  controls.set(select,{sync,menu,wrapper,position});sync();
 }
 function refresh(){
  root.querySelectorAll('select').forEach(enhance);
  for(const [select,control]of controls){if(!select.isConnected){control.menu.remove();control.wrapper.remove();controls.delete(select);}else control.sync();}
 }
 new MutationObserver(records=>{if(records.some(record=>!record.target.closest?.('.select-control,.select-menu')))refresh();}).observe(root.body||root,{subtree:true,childList:true,attributes:true,attributeFilter:['hidden','disabled','aria-label','aria-describedby','aria-invalid','required','data-loading','data-more','data-error']});
 addEventListener('resize',()=>{for(const control of controls.values())if(control.menu.matches(':popover-open'))control.position();});
 window.visualViewport?.addEventListener('resize',()=>{for(const control of controls.values())if(control.menu.matches(':popover-open'))control.position();});
 root.addEventListener('scroll',event=>{if(event.target.closest?.('.select-menu'))return;for(const control of controls.values())if(control.menu.matches(':popover-open'))control.position();},true);
 refresh();return refresh;
}
