// Keep raw chunks, including split JSON; only a bounded neighborhood stays in memory.
export class LogWindow {
 constructor(limit=48){this.limit=limit;this.events=[];this.older=false;this.newer=false;}
 replace(events,older=false){this.events=events;this.older=older;this.newer=false;}
 prepend(events,older){this.events=[...events,...this.events];this.older=older;if(this.events.length>this.limit){this.events=this.events.slice(0,this.limit);this.newer=true;}}
 append(events,newer){const ids=new Set(this.events.map(e=>e.id));this.events.push(...events.filter(e=>!ids.has(e.id)));this.newer=newer;if(this.events.length>this.limit){this.events=this.events.slice(-this.limit);this.older=true;}}
 get first(){return this.events[0]?.id;}
 get last(){return this.events.at(-1)?.id||0;}
}
