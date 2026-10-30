const sections=new Set(['pipelines','experiments','schedules','evidence','ci']);
const views=new Map(['Overview','Graph','Tests','Acceptance','Logs','Artifacts','Changes','Paths'].map(v=>[v.toLowerCase(),v]));
export function routePath(values){
 if(values.section==='executions'){const q=new URLSearchParams();for(const key of ['view','kind','project_id','query'])if(values[key])q.set(key,values[key]);return '/executions'+(q.size?'?'+q:'');}
 const enc=encodeURIComponent;
 let path='/projects/'+enc(values.project)+'/'+values.section;
 if(values.pipeline){path+='/'+enc(values.pipeline);if(values.run)path+='/runs/'+enc(values.run);path+='/'+(values.tab||'Graph').toLowerCase();}
 const params=new URLSearchParams();for(const key of ['stage','test'])if(values[key])params.set(key,values[key]);if(params.size)path+='?'+params;
 return path;
}
export function readRoute(url){
 const route=new URLSearchParams();
 try{
  if(url.pathname==='/'&&url.hash.startsWith('#project='))return new URLSearchParams(url.hash.slice(1));
  if(url.pathname==='/')return route;
  if(url.pathname==='/executions'){route.set('section','executions');for(const key of ['view','kind','project_id','query'])if(url.searchParams.has(key))route.set(key,url.searchParams.get(key));return route;}
  if(url.pathname==='/settings'){route.set('settings','true');return route;}
  const parts=url.pathname.replace(/\/$/,'').split('/').slice(1).map(decodeURIComponent);
  if(parts[0]!=='projects'||!parts[1]||!sections.has(parts[2]))throw Error('Unknown interface address');
  route.set('project',parts[1]);route.set('section',parts[2]);
  if(parts.length===3)return route;
  if(['schedules','evidence','ci'].includes(parts[2])||!parts[3])throw Error('Unknown interface address');
  route.set('pipeline',parts[3]);let tab;
  if(parts.length===5)tab=parts[4];
  else if(parts.length===7&&parts[4]==='runs'&&parts[5]){route.set('run',parts[5]);tab=parts[6];}
  else throw Error('Unknown interface address');
  if(!views.has(tab))throw Error('Unknown run view');route.set('tab',views.get(tab));
  for(const key of ['stage','test'])if(url.searchParams.has(key))route.set(key,url.searchParams.get(key));
 }catch(error){route.set('error',error.message);}
 return route;
}
