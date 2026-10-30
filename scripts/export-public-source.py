"""Export the reviewed source allowlist without local history, runtime data or internal evidence."""
import argparse,json,pathlib,re,shutil,subprocess
ROOT=pathlib.Path(__file__).resolve().parents[1]
ROOT_FILES={'.gitignore','Cargo.toml','Cargo.lock','README.md','LICENSE','SECURITY.md','CONTRIBUTING.md','PRIVACY.md','preflight.example.toml','install.sh'}
DIRECTORIES={'.github','assets','crates','docs','examples','integrations','skills','packaging','migrations','Formula'}
SCRIPTS={'release.py','release-files.py','installer-entry.py','smoke-package.py','test-release.py','install-preflight.py','client-notices.mjs','package-codex-plugin.py','test-codex-plugin.py','test-install-sh.py','test-client.py','test-client.cjs','test-launchd.py','accept-homebrew.py','export-public-source.py','test-public-export.py','homebrew-release.py'}
def export(destination):
 paths=subprocess.check_output(['git','ls-files','--cached','--others','--exclude-standard','-z'],cwd=ROOT).decode().split('\0')
 selected=[]
 for relative in sorted(set(paths)):
  path=pathlib.PurePosixPath(relative)
  if not relative or not (ROOT/relative).is_file():continue
  if not (relative in ROOT_FILES or path.parts[0] in DIRECTORIES or path.parts[0]=='scripts' and path.name in SCRIPTS):continue
  if path.parts[0]=='examples' and (len(path.parts)<2 or path.parts[1] not in {'go-basic','c-basic','cpp-basic'}):continue
  source=ROOT/relative
  if source.is_symlink():raise ValueError('Publication refuses symlink: '+relative)
  data=source.read_bytes()
  if (str(pathlib.Path.home()).encode()+b'/') in data or re.search(rb'gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|-----BEGIN (?:OPENSSH |RSA |EC )?PRIVATE KEY-----',data):raise ValueError('Review private data before export: '+relative)
  selected.append(relative)
 required=['Cargo.toml','.github/workflows/ci.yml','.github/workflows/release.yml','LICENSE','README.md','install.sh']
 assert all(p in selected for p in required)
 destination.mkdir(parents=True,exist_ok=False)
 for relative in selected:
  target=destination/relative;target.parent.mkdir(parents=True,exist_ok=True);shutil.copy2(ROOT/relative,target)

 plugin=destination/'integrations/codex'
 shutil.copytree(ROOT/'skills/preflight-mcp',plugin/'skills/preflight-mcp',dirs_exist_ok=True)
 (plugin/'assets').mkdir(exist_ok=True)
 shutil.copyfile(ROOT/'assets/branding/preflight-icon.png',plugin/'assets/icon.png')
 catalog=destination/'.agents/plugins/marketplace.json';catalog.parent.mkdir(parents=True)
 catalog.write_text(json.dumps({'name':'preflight','interface':{'displayName':'Preflight'},'plugins':[{'name':'preflight','source':{'source':'local','path':'./integrations/codex'},'policy':{'installation':'AVAILABLE','authentication':'ON_INSTALL'},'category':'Productivity'}]},indent=2)+'\n')
 for rust in (destination/'crates').rglob('*.rs'):
  for included in re.findall(r'include_(?:str|bytes)!\(\s*"([^"]+)"\s*\)',rust.read_text()):
   if not (rust.parent/included).is_file():raise ValueError('Missing compile-time input: '+str(rust.relative_to(destination))+' -> '+included)
 return selected
if __name__=='__main__':
 parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('destination',type=pathlib.Path);args=parser.parse_args()
 print(json.dumps({'files':len(export(args.destination)),'destination':str(args.destination.resolve()),'history_included':False}))
