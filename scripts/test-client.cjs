const { firefox } = require(process.env.PF_PLAYWRIGHT_MODULE || 'playwright');
const fs=require('fs');
(async()=>{
 let browser,page;
 process.once('SIGTERM',async()=>{try{await browser?.close();}finally{process.exit(143);}});
 try{
  browser=await firefox.launch({headless:true,...(process.env.PF_FIREFOX_EXECUTABLE ? {executablePath:process.env.PF_FIREFOX_EXECUTABLE} : {})});
  page=await browser.newPage({viewport:{width:1440,height:1000}});const errors=[];page.on('pageerror',e=>errors.push(e.message));
  const editCode=async text=>{await page.locator('#editor-code').click();const content=page.locator('#pipeline-code-editor .cm-content');await content.click();await page.keyboard.press('ControlOrMeta+A');await page.keyboard.insertText(text);await content.press('Tab');};
  const discardEditor=async()=>{const accept=dialog=>dialog.type()==='confirm'&&dialog.message().includes('Discard unsaved')?dialog.accept():dialog.dismiss();page.once('dialog',accept);await page.locator('#close-editor').click();await page.locator('#editor').waitFor({state:'hidden'});page.off('dialog',accept);};

  const choose=async(select,request)=>{
   const data=await select.evaluate(element=>{const native=element.tagName==='SELECT'?element:element.closest('.select-control').previousElementSibling;return {trigger:native.nextElementSibling.querySelector('button').id,options:[...native.options].map(o=>({value:o.value,label:o.textContent}))};});
   const option=typeof request==='string'?data.options.find(o=>o.value===request):request.index!==undefined?data.options[request.index]:data.options.find(o=>o.label===request.label);
   if(!option)throw Error('Missing choice '+JSON.stringify(request));
   await page.locator('#'+data.trigger).click();await page.locator('.select-menu:popover-open [role=option][data-value='+JSON.stringify(option.value)+']').click();
  };
  await page.goto(process.env.PF_CLIENT_URL);
  await page.locator('#catalog .catalog-card').first().waitFor();
  await page.locator('#layout-cards').click();await page.locator('.row-menu>summary').first().click();await page.locator('.row-menu>div:popover-open').waitFor();
  const menuBox=await page.locator('.row-menu>div:popover-open').boundingBox();if(menuBox.x<0||menuBox.y<0||menuBox.x+menuBox.width>1440)throw Error('Action menu outside viewport');
  await page.locator('.row-menu>div:popover-open').getByRole('button',{name:'Copy name',exact:true}).waitFor();await page.keyboard.press('Escape');await page.locator('#layout-list').click();

  await page.getByRole('button',{name:'Light theme',exact:true}).click();await page.screenshot({path:process.env.PF_EVIDENCE+'/catalog-light.png',fullPage:true});
  await page.getByRole('button',{name:'Dark theme',exact:true}).click();await page.screenshot({path:process.env.PF_EVIDENCE+'/catalog-dark.png',fullPage:true});
  if(await page.getByRole('button',{name:'Refresh',exact:true}).count())throw Error('Manual refresh still present');
  const headingBefore=await page.locator('#explorer-title').boundingBox();
  await page.locator('#new-pipeline').click();await page.locator('#pipeline-code-editor .cm-content').waitFor();if(await page.locator('#pipeline-code-editor .cm-lintRange-error').count())throw Error('Untouched new definition shows errors');if(!await page.locator('#save').isDisabled())throw Error('Empty draft can be saved');await editCode('{');await page.locator('#validate').click();await page.locator('#notifications .toast.error').waitFor({timeout:5000}).catch(async error=>{console.error(JSON.stringify({errors,notification:await page.locator('#notifications').evaluate(e=>e.outerHTML),editor:await page.locator('#editor').evaluate(e=>({open:e.open,hidden:e.hidden})),definition:await page.locator('#definition').inputValue()}));throw error;});
  if(!await page.locator('#notifications').evaluate(e=>e.parentElement.id==='editor'&&e.matches(':popover-open')))throw Error('Modal error notification inaccessible');
  await page.waitForTimeout(5200);if(await page.locator('#notifications .toast.error').count()!==1)throw Error('Error notification vanished');
  await page.getByRole('button',{name:'Dismiss notification',exact:true}).click();
  await editCode(JSON.stringify({version:1,pipeline:{stages:[{id:'check',command:{program:'echo',args:['ok']}}]}}));await page.locator('#validate').click();await page.getByText('Definition is valid.',{exact:true}).waitFor();await discardEditor();
  await page.locator('#notifications:popover-open').waitFor();const toastBox=await page.locator('#notifications').boundingBox();if(toastBox.x<1000||toastBox.y<500)throw Error('Notifications not in bottom-right corner');
  const headingAfter=await page.locator('#explorer-title').boundingBox();if(headingAfter.y!==headingBefore.y)throw Error('Notification moved page layout');
  await page.mouse.move(0,0);await page.locator('#notifications .toast').waitFor({state:'detached',timeout:7000});
  const workspaceUrl=page.url();const workspaceScroll=await page.evaluate(()=>window.scrollY);
  await page.getByRole('button',{name:'Settings',exact:true}).click();await page.getByRole('dialog',{name:'Settings',exact:true}).waitFor();if(page.url()!==workspaceUrl)throw Error('Settings changed workspace address');
  await page.keyboard.press('Escape');await page.getByRole('dialog',{name:'Settings',exact:true}).waitFor({state:'hidden'});if(page.url()!==workspaceUrl||await page.evaluate(()=>window.scrollY)!==workspaceScroll)throw Error('Settings lost workspace position');
  await page.getByRole('button',{name:'Settings',exact:true}).click();if(await page.getByLabel('Settings theme').count())throw Error('Theme remains in settings');if((await page.locator('#settings-open').innerText()).trim())throw Error('Settings icon has text');
  await page.getByLabel('Administrator token').fill(process.env.PF_TEST_ADMIN_TOKEN);await page.getByRole('button',{name:'Unlock settings',exact:true}).click();await page.getByLabel('Command slots',{exact:true}).fill('3');await page.getByLabel('Concurrent runs',{exact:true}).fill('4');
  await page.getByRole('button',{name:'Save settings',exact:true}).click();await page.getByText('Settings applied. Running pipelines were not interrupted.',{exact:true}).waitFor();
  await page.screenshot({path:process.env.PF_EVIDENCE+'/settings.png',fullPage:true});
  await page.getByRole('button',{name:'Close settings',exact:true}).click();
  const projectId=await page.locator('#projects').inputValue();const autoDefinition={"version":1,"pipeline":{"stages":[{"id":"live","command":{"program":"sh","args":["-c","printf 'first-live\\n'; i=1; while [ \"$i\" -le 80 ]; do printf 'console-event-%s <script>literal</script>\\n' \"$i\"; i=$((i+1)); sleep 0.02; done; sleep 3; printf 'second-live\\n'"],"timeout_seconds":15}}]}};
  const saved=await page.request.post(process.env.PF_CLIENT_URL+'/api/v1/pipeline-configs',{data:{project_id:projectId,name:'automatic-update',expected_revision:0,definition:autoDefinition}});if(!saved.ok())throw Error('Automatic update fixture failed');
  await page.locator('#pipelines button',{hasText:'automatic-update'}).waitFor({timeout:45000});
  await page.request.post(process.env.PF_CLIENT_URL+'/api/v1/saved-pipeline-runs',{data:{project_id:projectId,name:'automatic-update',revision:1}});
  await page.locator('#pipelines button',{hasText:'automatic-update'}).click();await page.locator('#tabs').getByRole('tab',{name:'Logs',exact:true}).click();await page.locator('.log-output').getByText('second-live',{exact:false}).waitFor({timeout:15000});
  const consoleView=page.locator('.log-output');
  if(!await consoleView.evaluate(e=>e.scrollHeight-e.scrollTop-e.clientHeight<40))throw Error('Console did not open at latest output');
  const historyRequests=[];page.on('request',request=>{if(request.url().includes('/evidence/search?'))historyRequests.push(request.url());});
  for(let n=0;n<12;n++){
   await consoleView.evaluate(e=>{e.scrollTop=0;e.dispatchEvent(new Event('scroll'));});
   await consoleView.hover();await page.mouse.wheel(0,-900);await page.waitForTimeout(150);
   if((await consoleView.innerText()).includes('console-event-1 <script>'))break;
  }
  await consoleView.getByText('console-event-1 <script>literal</script>',{exact:true}).waitFor();
  if(!historyRequests.some(url=>new URL(url).searchParams.has('before')))throw Error('Scrolling did not fetch older output');
  if(await consoleView.locator('script').count())throw Error('Log interpreted as HTML');
  if(await consoleView.locator('.console-entry').count()>100)throw Error('Console DOM is not bounded');
  await page.getByRole('button',{name:'Resume following',exact:true}).waitFor();
  await page.getByRole('button',{name:'Jump to end',exact:true}).click();
  await consoleView.getByText('second-live',{exact:true}).waitFor();
  if(!await consoleView.evaluate(e=>e.scrollHeight-e.scrollTop-e.clientHeight<40))throw Error('Console follow did not resume');
  await page.screenshot({path:process.env.PF_EVIDENCE+'/live-console.png',fullPage:true});



await choose(page.locator('#projects'),{label:'Project Patch'});await page.locator('#pipelines button').first().click();await page.locator('#run-state').getByText('succeeded',{exact:true}).waitFor();
  await page.locator('#tabs').getByRole('tab',{name:'Changes',exact:true}).click();await page.locator('.diff-file').waitFor();
  if(await page.locator('.diff-added .diff-code').innerText()!=='+after <script>alert(1)</script>')throw Error('Patch text mismatch');
  if(await page.locator('.diff-removed .diff-code').innerText()!=='-before')throw Error('Removed source missing');
  if(await page.locator('.diff-added .line-number').last().innerText()!=='1')throw Error('New line number mismatch');
  if(await page.locator('.diff-file script').count())throw Error('Source interpreted as HTML');
  await page.getByRole('button',{name:'Collapse files',exact:true}).click();if(await page.locator('.diff-file').evaluate(e=>e.open))throw Error('Collapse failed');
  await page.getByRole('button',{name:'Expand files',exact:true}).click();await page.getByLabel('Filter changed files').fill('absent');if(await page.locator('.diff-file').isVisible())throw Error('File filter failed');await page.getByLabel('Filter changed files').fill('sample');
  for(const theme of ['Light theme','Dark theme']){await page.getByRole('button',{name:theme,exact:true}).click();await page.screenshot({path:process.env.PF_EVIDENCE+'/code-diff-'+theme.split(' ')[0]+'.png',fullPage:true});}
  await choose(page.locator('select[aria-label="Baseline run"]'),{index:1});await page.getByRole('button',{name:'Compare runs',exact:true}).click();await page.locator('.comparison-file>summary').getByText('sample.txt · modified',{exact:true}).click();await page.locator('.comparison-file .diff-added').waitFor();if(!(await page.locator('.comparison-file .diff-removed').innerText()).includes('-before'))throw Error('Captured comparison omitted old code');await page.screenshot({path:process.env.PF_EVIDENCE+'/snapshot-diff.png',fullPage:true});

  await choose(page.locator('#projects'),{label:'Project A'});await page.locator('#pipelines button',{hasText:'test-suite'}).click();
  await page.locator('#run-state').getByText('succeeded',{exact:true}).waitFor();
  await page.locator('#tabs').getByRole('tab',{name:'Graph',exact:true}).click();await page.locator('#view .stage').first().waitFor();
  if(await page.locator('#view .stage').count()!==3)throw Error('Expected three DAG blocks');
  await page.locator('#view .stage').getByRole('button',{name:'prepare',exact:true}).click();
  await page.locator('#view .stage-inspector').getByRole('heading',{name:'Command',exact:true}).waitFor();
  if(!(await page.locator('#view .stage-inspector').innerText()).includes('searchable acceptance marker'))throw Error('Stage command missing');
  await page.getByRole('slider',{name:'Graph zoom'}).focus();await page.keyboard.press('End');if(await page.getByRole('slider',{name:'Graph zoom'}).inputValue()!=='140')throw Error('Keyboard zoom failed');
  await page.locator('.graph-toolbar').getByRole('button',{name:'Reset view'}).click();
  await page.locator('#view .stage-inspector').getByRole('button',{name:'Open stage logs'}).click();
  await page.getByPlaceholder('Search this run’s logs').waitFor();
  await page.locator('#history-toggle').click();if(!await page.locator('#history-panel').isVisible())throw Error('History did not open');
  await page.locator('#history-toggle').click();if(await page.locator('#history-panel').isVisible())throw Error('History did not hide');
  await page.locator('#pipeline-filter').fill('missing');if(await page.locator('#pipelines button:visible').count() > 0)throw Error('Pipeline filter did not apply');await page.locator('#pipeline-filter').fill('');

  await page.locator('#tabs').getByRole('tab',{name:'Tests',exact:true}).click();await page.locator('.tree-row').getByRole('button',{name:'sample',exact:true}).click();
  await page.locator('#test-content').getByText('Current checkout matches the executed source.').waitFor();
  if(!(await page.locator('#test-content').innerText()).includes('fn sample'))throw Error('Source missing');
  const uri=await page.locator('#test-content a').getAttribute('href');if(!uri.startsWith('vscode://file/'))throw Error('Unsafe/missing editor URI');
  await page.locator('#close-test').click();
  if(!new URL(page.url()).pathname.endsWith('/tests'))throw Error('Run route missing');
  await page.reload();await page.locator('.tree-row').getByRole('button',{name:'sample',exact:true}).waitFor();
  await page.locator('#sections button[data-section=schedules]').click();await page.locator('#explorer-title').getByText('Schedules',{exact:true}).waitFor();
  await page.goBack();await page.locator('.tree-row').getByRole('button',{name:'sample',exact:true}).waitFor();

  await page.getByRole('searchbox',{name:'Find a test'}).fill('not-found');
  await page.getByText('No tests match these filters.',{exact:true}).waitFor();
  await page.getByRole('searchbox',{name:'Find a test'}).fill('sample');
  await choose(page.locator('select[aria-label="Test status"]'),'failed');
  await page.getByText('No tests match these filters.',{exact:true}).waitFor();
  await choose(page.locator('select[aria-label="Test status"]'),'all');
  await page.getByRole('button',{name:'Collapse all',exact:true}).click();
  if(await page.locator('.tree-row').isVisible())throw Error('Tree did not collapse');
  await page.getByRole('button',{name:'Expand all',exact:true}).click();
  await page.locator('.tree-row').waitFor();

  await page.getByRole('button',{name:'Light theme',exact:true}).click();await page.screenshot({path:process.env.PF_EVIDENCE+'/light.png',fullPage:true});
  await page.getByRole('button',{name:'Dark theme',exact:true}).click();await page.screenshot({path:process.env.PF_EVIDENCE+'/dark.png',fullPage:true});
  await page.locator('#tabs').getByRole('tab',{name:'Graph',exact:true}).click();
  await page.locator('#tab-graph').press('ArrowRight');
  if(await page.locator('#tab-tests').getAttribute('aria-selected')!=='true')throw Error('Arrow keys did not select the next tab');
  if(!await page.locator('#tab-tests').evaluate(e=>e===document.activeElement))throw Error('Tab focus was lost');
  await page.locator('#tab-tests').press('End');
  if(await page.locator('#tab-paths').getAttribute('aria-selected')!=='true')throw Error('End did not select the last tab');
  if(await page.locator('#view').getAttribute('aria-labelledby')!=='tab-paths')throw Error('Panel is not labelled by its tab');
  await page.locator('#tabs').getByRole('tab',{name:'Paths',exact:true}).click();await page.getByRole('heading',{name:'Run paths'}).waitFor();
  await page.locator('#tabs').getByRole('tab',{name:'Logs',exact:true}).click();await page.getByPlaceholder('Search this run’s logs').fill('searchable');await page.getByRole('button',{name:'Search',exact:true}).click();await page.locator('#view .card').getByText('searchable acceptance marker',{exact:false}).waitFor();
  await page.locator('#tabs').getByRole('tab',{name:'Artifacts',exact:true}).click();await page.locator('#view summary').getByText('prepare / result · artifact_expired',{exact:true}).waitFor();
  await page.locator('#edit').click();await page.locator('#editor-visual').click();await page.getByRole('button',{name:'Add stage',exact:true}).click();if(await page.locator('.stage-form').count()!==4)throw Error('Add stage failed');await page.locator('.stage-form').last().getByLabel('prepare',{exact:true}).check();if(!JSON.parse(await page.locator('#definition').inputValue()).pipeline.stages[3].needs.includes('prepare'))throw Error('Dependency editor failed');await page.locator('.stage-form').last().getByRole('button',{name:'Remove stage'}).click();if(await page.locator('.stage-form').count()!==3)throw Error('Remove stage failed');await page.locator('#editor').evaluate(e=>e.scrollTop=0);await page.screenshot({path:process.env.PF_EVIDENCE+'/editor.png',fullPage:true});await page.getByLabel('Timeout (seconds)',{exact:true}).first().fill('6');await page.getByLabel('Program',{exact:true}).first().click();if(JSON.parse(await page.locator('#definition').inputValue()).pipeline.stages[0].command.timeout_seconds!==6)throw Error('Visual editor did not sync JSON');await page.locator('#validate').click();await page.getByText('Definition is valid.',{exact:true}).waitFor();
  await page.locator('#save').click();await page.getByText('Saved immutable revision 2',{exact:true}).waitFor();
  await page.locator('#sections button[data-section=schedules]').click();await page.locator('#new-schedule').click();await page.locator('#schedule-name').fill('later');await choose(page.locator('#schedule-repeat'),'interval');await page.locator('#schedule-interval').fill('5');await choose(page.locator('#schedule-repeat'),'calendar');await choose(page.locator('#schedule-timezone'),'Asia/Yekaterinburg');await page.locator('#schedule-preview').click();await page.locator('#schedule-preview-times li').nth(4).waitFor();await page.locator('#schedule-form').getByRole('button',{name:'Create schedule',exact:true}).click();await page.getByText('Schedule created.',{exact:true}).waitFor();
  const scheduleChange=async(label,expected)=>{const reply=page.waitForResponse(r=>r.url().endsWith('/api/v1/schedules/change'));await page.locator('#schedules').getByRole('button',{name:label,exact:true}).click();const response=await reply;const value=await response.json();if(!response.ok()||value.state!==expected)throw Error('Schedule '+label+': '+JSON.stringify(value));await page.locator('#schedules .badge.'+expected).waitFor();};
  await scheduleChange('Pause','paused');await scheduleChange('Resume','pending');await scheduleChange('Cancel schedule','cancelled');await page.locator('#schedules').getByRole('button',{name:'History',exact:true}).click();await page.getByRole('dialog',{name:'Schedule history'}).getByText('No occurrences yet.').waitFor();await page.getByRole('dialog',{name:'Schedule history'}).getByRole('button',{name:'Close',exact:true}).click();await page.locator('#schedules').getByRole('button',{name:'Delete schedule',exact:true}).click();await page.locator('#schedules .card').waitFor({state:'detached'});
  await page.locator('#sections button[data-section=experiments]').click();await page.locator('#new-experiment').click();await page.locator('#pipeline-name').fill('trial');await page.getByRole('button',{name:'Customize stages',exact:true}).click();await page.locator('.experiment-overrides .stage-form').first().waitFor();await choose(page.locator('select[aria-label="Source snapshot"]'),'git');await page.getByLabel('Exact commit SHA',{exact:true}).fill('a'.repeat(40));await page.getByRole('button',{name:'Add patch',exact:true}).click();await page.getByLabel('Patch 1 unified diff',{exact:true}).fill('test patch text');await choose(page.locator('select[aria-label="Source snapshot"]'),'current');await page.locator('#save').click();await page.getByText('Saved immutable revision 1',{exact:true}).waitFor();
  await page.locator('#start').click();await page.locator('#run-state').getByText('succeeded',{exact:true}).waitFor();
  if(await page.locator('#history tr').count()!==2)throw Error('Experiment history should contain one run');
  await page.locator('#sections button[data-section=pipelines]').click();await page.locator('#pipelines button',{hasText:'test-suite'}).click();await page.locator('#run-state').getByText('succeeded',{exact:true}).waitFor();
  if(await page.locator('#history tr').count()!==2)throw Error('Experiment leaked into normal pipeline history');
  await choose(page.locator('#projects'),{label:'Project B'});await page.locator('#pipelines button',{hasText:'test-suite'}).click();await page.locator('#run-state').getByText('failed',{exact:true}).waitFor();
  await page.locator('#tabs').getByRole('tab',{name:'Tests',exact:true}).click();await page.locator('.tree-row .failed').waitFor();
  if(await page.locator('.tree-row .passed').count())throw Error('Cross-project test status leaked');
  for(const appearance of ['light','dark']){
   await page.getByRole('button',{name:appearance==='light'?'Light theme':'Dark theme',exact:true}).click();
   for(const view of ['Overview','Graph','Tests','Logs','Artifacts','Changes','Paths']){
    await page.locator('#tabs').getByRole('tab',{name:view,exact:true}).click();await page.waitForLoadState('networkidle');
    if(await page.evaluate(()=>document.documentElement.scrollHeight>innerHeight+1))throw Error('Page height grows in '+view);
    if(view==='Changes'){
     await page.waitForFunction(()=>document.querySelector('select[aria-label=\"Baseline run\"]')?.options[0]?.textContent==='Select a baseline run…');
     if(!await page.getByRole('button',{name:'Compare runs',exact:true}).isDisabled())throw Error('Comparison enabled without a baseline');
    }
    await page.evaluate(()=>scrollTo(0,0));await page.screenshot({path:process.env.PF_EVIDENCE+'/'+appearance+'-'+view.toLowerCase()+'.png',fullPage:true});
   }
  }
  await choose(page.locator('#projects'),{label:'Project Parallel'});await page.locator('#pipelines button',{hasText:'parallel-check'}).click();
  await page.locator('#tabs').getByRole('tab',{name:'Graph',exact:true}).click();await page.getByRole('button',{name:'Fit to screen',exact:true}).click();
  await page.waitForFunction(()=>{const v=document.querySelector('.graph-viewport').getBoundingClientRect(),nodes=[...document.querySelectorAll('#view .stage')];return nodes.length===11&&nodes.every(n=>{const b=n.getBoundingClientRect();return b.left>=v.left-2&&b.right<=v.right+2&&b.top>=v.top-2&&b.bottom<=v.bottom+2;});});
  await page.screenshot({path:process.env.PF_EVIDENCE+'/parallel-11-stages.png',fullPage:true});
  await page.locator('#tabs').getByRole('tab',{name:'Overview',exact:true}).click();if(await page.locator('.stage-table tr').count()!==12)throw Error('Stage overview missing rows');
  const previous=await page.locator('#run-picker option').nth(1).getAttribute('value');await choose(page.locator('#run-picker'),previous);await page.locator('#run-title').getByText('Run '+previous.slice(-8),{exact:true}).waitFor();
  await choose(page.locator('#projects'),{label:'Project B'});await page.locator('#pipelines button',{hasText:'test-suite'}).click();
  await page.locator('#tabs').getByRole('tab',{name:'Graph',exact:true}).click();await page.evaluate(()=>scrollTo(0,0));await page.screenshot({path:process.env.PF_EVIDENCE+'/graph.png',fullPage:true});
  await page.setViewportSize({width:390,height:844});await page.evaluate(()=>scrollTo(0,0));
  await page.locator('#sidebar').waitFor({state:'hidden'});
  await page.locator('#sidebar-toggle').click();
  await page.locator('#sidebar').waitFor({state:'visible'});
  await page.screenshot({path:process.env.PF_EVIDENCE+'/mobile-navigation.png',fullPage:true,animations:'disabled'});
  if(!await page.locator('#sidebar').evaluate(e=>{const r=e.getBoundingClientRect();return r.left>=0&&r.right<=innerWidth;}))throw Error('Mobile navigation leaves the viewport');
  await page.locator('#sidebar-toggle').click();await page.locator('#sidebar').waitFor({state:'hidden'});
  await page.screenshot({path:process.env.PF_EVIDENCE+'/mobile.png',fullPage:true});
  if(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth+1))throw Error('Mobile page overflows horizontally');
  await page.setViewportSize({width:1440,height:1000});

  let releaseStart,markStarted;
  const gate=new Promise(resolve=>releaseStart=resolve),started=new Promise(resolve=>markStarted=resolve);
  await page.route('**/api/v1/saved-pipeline-runs',async route=>{const response=await route.fetch();markStarted();await gate;await route.fulfill({response});});
  await page.locator('#start').click();await started;
  await choose(page.locator('#projects'),{label:'Project A'});
  const response=page.waitForResponse(r=>r.url().endsWith('/api/v1/saved-pipeline-runs'));releaseStart();await response;
  await page.waitForFunction(()=>!document.querySelector('#start').hasAttribute('aria-busy'));
  if(await page.locator('#title').innerText()!=='Project pipelines')throw Error('Late start response changed project selection');
  await page.unroute('**/api/v1/saved-pipeline-runs');
  await page.waitForFunction(()=>location.pathname.includes('/projects/'+document.getElementById('projects').value+'/'));await page.waitForLoadState('networkidle');
  await page.setViewportSize({width:850,height:900});const menuUrl=page.url();
  await page.getByRole('button',{name:'Hide menu',exact:true}).click();await page.locator('#sidebar').waitFor({state:'hidden'});if(await page.locator('#sidebar-toggle').getAttribute('aria-expanded')!=='false')throw Error('Wrong collapsed state');
  await page.setViewportSize({width:390,height:844});await page.getByRole('button',{name:'Show menu',exact:true}).click();await page.locator('#sidebar').waitFor();await page.keyboard.press('Escape');await page.locator('#sidebar').waitFor({state:'hidden'});
  await page.getByRole('button',{name:'Show menu',exact:true}).click();await page.locator('#nav-backdrop').click({position:{x:350,y:100}});await page.locator('#sidebar').waitFor({state:'hidden'});
  await page.setViewportSize({width:850,height:900});await page.locator('#sidebar').waitFor({state:'hidden'});await page.getByRole('button',{name:'Show menu',exact:true}).click();if(page.url()!==menuUrl)throw Error('Menu changed address');
  await page.setViewportSize({width:1440,height:1000});
  await page.locator('#layout-cards').click();if(await page.locator('#catalog').evaluate(e=>e.classList.contains('list-layout')))throw Error('Cards toggle failed');await page.locator('#layout-list').click();
  await page.locator('#catalog-search').fill('missing');await page.locator('#catalog-empty').waitFor();await page.locator('#catalog-search').fill('');
  await page.locator('#catalog .row-menu summary').first().click();await page.locator('#catalog .row-menu').getByRole('button',{name:'Copy name',exact:true}).waitFor();await page.locator('#explorer-title').click();
  await page.evaluate(()=>Object.defineProperty(navigator,'clipboard',{configurable:true,value:{writeText:async()=>{throw Error('blocked for acceptance');}}}));
  await page.locator('#catalog .row-menu summary').first().click();await page.locator('#catalog .row-menu').first().getByRole('button',{name:'Copy name',exact:true}).click();await page.getByRole('dialog',{name:'Copy data'}).waitFor();if(!(await page.getByLabel('Data to copy').inputValue()).length)throw Error('Copy fallback lost data');await page.locator('#copy-dialog').getByRole('button',{name:'Close',exact:true}).click();

  if(errors.length)throw Error(errors.join('\n'));
  fs.writeFileSync(process.env.PF_EVIDENCE+'/browser.json',JSON.stringify({passed:true,bundled:process.env.PF_BUNDLED==='true',browser:'headless Firefox',projects:4,dagBlocks:3,source:true,vscode:true,themes:['light','dark'],editorRevision:2,projectIsolation:true,experimentHistory:true,graphInspector:true,zoom:true,testFilters:true,visualEditor:true,mobileOverflow:false,lateStartIsolation:true,scheduleMutations:true,realLogHistory:true,consoleDomBounded:true,copyFallback:true,errors},null,2));
  console.log('Browser acceptance passed');
 }catch(error){if(page){await page.screenshot({path:process.env.PF_EVIDENCE+'/failure.png',fullPage:true}).catch(()=>{});console.error('Failed at',page.url());}throw error;}finally{if(browser)await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;});
