const messageBrand=Symbol('Preflight UI message');
const textBindings=new WeakMap(),attributeBindings=new WeakMap();
function render(key,values={}){
 return key.replace(/\{(\d+)\}/g,(token,index)=>Object.hasOwn(values,index)?String(values[index]??''):token);
}
export function msg(key,values={}){
 return {[messageBrand]:true,key,values,toString(){return render(key,values);},toJSON(){throw Error('Localized UI messages cannot be serialized as API data');}};
}
const isMessage=value=>Boolean(value?.[messageBrand]);
function bindText(node,value){textBindings.set(node,value);node.dataset.i18n=value.key;node.textContent=String(value);return node;}
export function uiNode(value){return isMessage(value)?bindText(document.createElement('span'),value):document.createTextNode(String(value??''));}
export function put(node,value){
 textBindings.delete(node);node.removeAttribute('data-i18n');
 if(isMessage(value)){
  if(node.tagName==='OPTION'||node.namespaceURI==='http://www.w3.org/2000/svg')bindText(node,value);
  else node.replaceChildren(uiNode(value));
 }else node.textContent=String(value??'');
 return value;
}
export function attr(node,name,value){
 let bindings=attributeBindings.get(node);
 if(isMessage(value)){if(!bindings){bindings=new Map();attributeBindings.set(node,bindings);}bindings.set(name,value);node.setAttribute('data-i18n-attrs','');node.setAttribute(name,String(value));}
 else{bindings?.delete(name);if(!bindings?.size)node.removeAttribute('data-i18n-attrs');node.setAttribute(name,String(value??''));}
 return value;
}
export function choice(text='',value,defaultSelected=false,selected=false){
 const original=isMessage(text)?render(text.key,text.values):String(text);
 const node=new Option(original,value===undefined?original:value,defaultSelected,selected);put(node,text);return node;
}
export function cloneChoice(option){if(!option)return undefined;const copy=option.cloneNode(true),binding=textBindings.get(option);if(binding)bindText(copy,binding);return copy;}
export function formatted(value,method='toLocaleString',options){
 const date=value instanceof Date;
 return msg('{0}',{0:{toString:()=>value[method](date?'en-GB':'en',date&&method!=='toLocaleDateString'?{...options,hourCycle:'h23'}:options)}});
}
export function timestamp(value){
 if(value===null||value===undefined)return '—';
 const date=new Date(value);if(!Number.isFinite(date.getTime()))return '—';
 const parts=Object.fromEntries(new Intl.DateTimeFormat('en-GB',{day:'2-digit',month:'2-digit',year:'numeric',hour:'2-digit',minute:'2-digit',second:'2-digit',hourCycle:'h23'}).formatToParts(date).map(part=>[part.type,part.value]));
 return `${parts.day}.${parts.month}.${parts.year} ${parts.hour}:${parts.minute}:${parts.second}`;
}
export function uiError(value){const error=Error(String(value));if(isMessage(value))error.localizedMessage=value;return error;}
export function statusLabel(value){return value;}
function apply(root){
 for(const node of root.querySelectorAll('[data-i18n]')){const binding=textBindings.get(node)||msg(node.dataset.i18n);if(node.tagName==='OPTION'&&!node.hasAttribute('value'))node.value=node.textContent;textBindings.set(node,binding);node.textContent=String(binding);}
 for(const node of root.querySelectorAll('[data-i18n-title],[data-i18n-placeholder],[data-i18n-aria-label]'))for(const name of ['title','placeholder','aria-label']){const key=node.getAttribute('data-i18n-'+name);if(key!==null){attr(node,name,msg(key));node.removeAttribute('data-i18n-'+name);}}
 for(const node of root.querySelectorAll('[data-i18n-attrs]'))for(const[name,binding]of attributeBindings.get(node)||[])node.setAttribute(name,String(binding));
 root.documentElement.lang='en';
}
export function installLocalization(root=document){apply(root);try{globalThis.localStorage?.removeItem('preflight:language');}catch{}}
