import {registerPageButton} from './virtual-list.mjs';
const sections=[
 ['projects','Projects',['project']],['pipelines','Pipelines',['pipeline']],
 ['experiments','Experiments',['experiment']],['runs','Runs',['run']],
 ['stages','Stages',['stage']],['tests','Tests',['test']],
 ['artifacts','Artifacts',['artifact','module']],['files','Files',['input']],
 ['changes','Source changes',['source']],['keys','Stage keys',['stagekey','cache']],
 ['logs','Logs',['log']],['other','Other',[]],
];
export function groupSearchResults(items){
 const groups=sections.map(([key,label,kinds])=>({key,label,kinds,items:[]}));
 for(const item of items)(groups.find(group=>group.kinds.includes(item.kind))||groups.at(-1)).items.push(item);
 return groups.filter(group=>group.items.length);
}
// One read-only search surface; the same endpoint is available through MCP search_global.
export function installGlobalSearch({api,el,routePath,navigate}){
 const form=document.getElementById('global-search'),input=document.getElementById('global-search-input'),popup=document.getElementById('global-search-results');
 let timer,epoch=0,items=[],selected=0,cursor=null,busy=false;
 const status=el('p','Type a name, UUID or hash','global-search-status');status.setAttribute('role','status');
 const list=el('div');list.id='global-search-list';list.setAttribute('role','listbox');list.setAttribute('aria-label','Search results');
 const more=el('button','Load more','global-search-more');more.type='button';more.hidden=true;
 popup.append(status,list,more);input.setAttribute('aria-controls',list.id);
 function position(){const r=form.getBoundingClientRect(),width=Math.min(Math.max(r.width,320),innerWidth-16);popup.style.width=width+'px';popup.style.left=Math.max(8,Math.min(r.left,innerWidth-width-8))+'px';popup.style.top=r.bottom+6+'px';popup.style.maxHeight=Math.max(100,innerHeight-r.bottom-22)+'px';}
 function open(){if(!popup.matches(':popover-open'))popup.showPopover();position();}
 function select(index,reveal=true){selected=Math.max(0,Math.min(items.length-1,index));const options=[...list.querySelectorAll('[role=option]')];for(const [i,row]of options.entries())row.setAttribute('aria-selected',String(i===selected));if(items.length){input.setAttribute('aria-activedescendant','global-result-'+selected);if(reveal)options[selected]?.scrollIntoView({block:'nearest'});}else input.removeAttribute('aria-activedescendant');}
 async function choose(item){popup.hidePopover();input.blur();await navigate(item.target);}
 function render(keepSelection=false){
 const activeKey=keepSelection?items[selected]?.key:null,groups=groupSearchResults(items);items=groups.flatMap(group=>group.items);selected=activeKey?Math.max(0,items.findIndex(item=>item.key===activeKey)):0;list.replaceChildren();let index=0;
 for(const group of groups){const section=el('div');section.className='global-search-group';section.setAttribute('role','group');const title=el('div',undefined,'global-search-group-title');title.id='global-group-'+group.key;title.append(el('span',group.label),el('span',String(group.items.length),'muted'));section.setAttribute('aria-labelledby',title.id);section.append(title);
 for(const item of group.items){const i=index++,row=el('a');row.id='global-result-'+i;row.className='global-search-hit';row.href=routePath(item.target);row.setAttribute('role','option');row.setAttribute('aria-selected',String(i===selected));row.tabIndex=-1;
 const body=el('span',undefined,'global-search-body');body.append(el('strong',item.title),el('span',[item.project_name,item.target.run?.slice(-8),item.target.stage,item.detail].filter(Boolean).join(' · '),'muted'));row.title=[item.title,item.id,item.detail].join('\n');row.append(body);row.addEventListener('pointermove',()=>select(i));row.onclick=event=>{if(event.button||event.metaKey||event.ctrlKey||event.shiftKey||event.altKey)return;event.preventDefault();choose(item).catch(error=>{open();status.textContent=error.message;});};section.append(row);
 }list.append(section);
 }select(selected,!keepSelection);more.hidden=!cursor;
 }

 async function search(append=false){const q=input.value.trim(),generation=append?epoch:++epoch;if(!append){items=[];selected=0;cursor=null;render();}if(q.length<2&&!/^#?\d+$/.test(q)){status.textContent='Type a name, UUID or hash';return;}
 if(append&&busy)return;busy=true;more.dataset.pageError='';more.textContent='Load more';more.disabled=true;input.setAttribute('aria-busy','true');status.textContent=append?'Loading more…':'Searching…';open();
 try{const params=new URLSearchParams({query:q,limit:'20'});if(append&&cursor)params.set('after',cursor);const result=await api('search?'+params);if(generation!==epoch)return;const seen=new Set(items.map(i=>i.key));items.push(...result.results.filter(i=>!seen.has(i.key)));cursor=result.next_cursor;status.textContent=items.length?`${items.length} results${cursor?' · more available':''}`:'No results';if(result.unavailable_projects?.length)status.textContent+=' · Some project configurations are unavailable';render(append);position();}
 catch(error){if(generation===epoch){status.textContent=error.message;more.hidden=false;more.textContent='Retry';more.dataset.pageError=error.message;}}
 finally{if(generation===epoch){busy=false;more.disabled=false;input.removeAttribute('aria-busy');}}
 }
 input.addEventListener('input',()=>{clearTimeout(timer);epoch++;busy=false;input.removeAttribute('aria-busy');items=[];cursor=null;selected=0;render();status.textContent='Searching…';open();timer=setTimeout(()=>search(),220);});
 input.addEventListener('focus',()=>{open();if(input.value.trim()&&!items.length&&!busy)search();});input.addEventListener('click',open);
 input.addEventListener('keydown',event=>{if(event.key==='Escape'){event.preventDefault();clearTimeout(timer);epoch++;busy=false;input.removeAttribute('aria-busy');popup.hidePopover();input.blur();}else if(['ArrowDown','ArrowUp','Home','End'].includes(event.key)){event.preventDefault();open();select(event.key==='Home'?0:event.key==='End'?items.length-1:selected+(event.key==='ArrowDown'?1:-1));}else if(event.key==='Enter'){event.preventDefault();clearTimeout(timer);if(items[selected])choose(items[selected]).catch(error=>{open();status.textContent=error.message;});else search();}});
 form.addEventListener('submit',event=>event.preventDefault());more.onclick=registerPageButton(more,()=>search(Boolean(cursor)));
 popup.addEventListener('toggle',event=>input.setAttribute('aria-expanded',String(event.newState==='open')));
 document.addEventListener('keydown',event=>{if(document.querySelector('dialog[open]'))return;const shortcut=(event.metaKey||event.ctrlKey)&&event.key.toLowerCase()==='k',slash=event.key==='/'&&!event.target.closest('input,textarea,[contenteditable],.cm-editor');if(shortcut||slash){event.preventDefault();input.focus();input.select();open();}});
 addEventListener('resize',()=>{if(popup.matches(':popover-open'))position();});
 return {close(){clearTimeout(timer);epoch++;popup.hidePopover();}};
}
