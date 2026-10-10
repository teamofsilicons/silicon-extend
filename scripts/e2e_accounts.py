#!/usr/bin/env python3
"""Real Accounts + Extend + native-device protocol + CLI rehearsal. Local stack only.
EXTEND_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts \
  CARGO_TARGET_DIR=target/mig python3 scripts/e2e_accounts.py
Secrets stay under ignored .mig, never in test output. Dev stack must be running.
"""
import base64,json,os,re,subprocess,time,uuid,sys
from pathlib import Path
from urllib.request import Request,urlopen
from urllib.error import HTTPError
ROOT=Path(__file__).resolve().parents[1]
RUN=str(int(time.time()))
OUT=ROOT/'.mig'/'accounts-e2e'/RUN;OUT.mkdir(parents=True,exist_ok=True)
STACK=json.loads(Path(os.environ['EXTEND_TEST_STACK']).read_text())
ACCOUNTS=STACK['accounts_api_url'];API='http://127.0.0.1:4221'
assert ACCOUNTS.startswith(('http://127.0.0.1:','http://localhost:'))
BIN=Path(os.environ.get('CARGO_TARGET_DIR',str(ROOT/'target/mig'))).resolve()/'debug'
SA=Path(os.environ['SILICON_ACCOUNTS_DIR']);CHECKS=[]
def check(name,ok):
 CHECKS.append({'name':name,'passed':bool(ok)});print(('PASS ' if ok else 'FAIL ')+name,flush=True)
 if not ok:raise AssertionError(name)
def request(url,method='GET',token=None,body=None,headers=None):
 h={'Content-Type':'application/json',**(headers or {})}
 if token:h['Authorization']='Bearer '+token
 try:
  r=urlopen(Request(url,method=method,headers=h,data=json.dumps(body).encode() if body is not None else None),timeout=30)
 except HTTPError as e:r=e
 raw=r.read()
 try:value=json.loads(raw) if raw else None
 except ValueError:value=raw
 return r.status,value
def api(path,token,method='GET',data=None,kind=None,headers=None):
 return request(API+'/api/v2/'+path,method,token,{'type':kind or path.split('/')[0],'data':data} if data is not None else None,headers)
def mint(*args):
 p=subprocess.run([str(SA/'testkit/node_modules/.bin/tsx'),str(ROOT/'scripts/mint-accounts.mts'),*args],capture_output=True,text=True,timeout=120)
 if p.returncode:raise RuntimeError('Accounts identity setup failed: '+p.stderr[-300:])
 return json.loads(p.stdout)
def cli(home,*args,input=None,ok=True):
 env={**os.environ,'SILICON_HOME':str(OUT/home),'EXTEND_API_URL':API,'ACCOUNTS_URL':STACK['accounts_public_url'],'EXTEND_TELEMETRY':'off'}
 p=subprocess.run([str(BIN/'extend'),*args],env=env,input=input,capture_output=True,text=True,timeout=40)
 if ok and p.returncode:raise RuntimeError('CLI '+args[0]+' failed: '+p.stderr[-500:])
 return p
def wait(fn,seconds=20):
 end=time.time()+seconds
 while time.time()<end:
  value=fn()
  if value:return value
  time.sleep(.3)
 raise TimeoutError('Expected state did not arrive')
def carbon(who):
 email=f'extend-e2e-{who}-{RUN}@example.test';first=mint('carbon','--email',email)
 tokens=mint('app-signin','--app','extend','--email',email,'--redirect','http://127.0.0.1:4220/auth/callback','--exchange','--existing')['tokens']
 return {**first,'email':email,'tokens':tokens}
DEVICE=None
COMPLETE=False
try:
 c=carbon('owner');other=carbon('other');ct=c['tokens']['access_token'];ot=other['tokens']['access_token']
 check('Carbon hosted sign-in accepted as exact Accounts uuid',api('me',ct)[1]['data']['uuid']==c['uuid'])
 check('Wrong audience refused',api('me',c['access_token'])[0]==401)
 s,created=request(ACCOUNTS+'/v1/me/silicons','POST',c['access_token'],{'id':f'si:extend-e2e-{RUN}','display_name':'Extend rehearsal'}, {'Idempotency-Key':str(uuid.uuid4())});check('Silicon created under custodian',s==201)
 silicon=created['silicon'];slt=mint('slt','--silicon',silicon['id'],'--stk',created['stk'],'--app','extend')['slt']
 cli('silicon','login','--slt-stdin',input=slt+'\n');status=json.loads(cli('silicon','login','status','--json').stdout);check('Silicon CLI sign-in and custody',status['authenticated'] and status['uuid']==silicon['uuid'])
 # First-party device flow approval uses the Carbon's existing token and spends no email code.
 env={**os.environ,'SILICON_HOME':str(OUT/'carbon-cli'),'EXTEND_API_URL':API,'ACCOUNTS_URL':STACK['accounts_public_url'],'EXTEND_TELEMETRY':'off'}
 log=OUT/'device-flow.log'
 with log.open('w') as f:login=subprocess.Popen([str(BIN/'extend'),'login'],env=env,stdout=f,stderr=f)
 code=wait(lambda: (re.search(r'\b[A-Z0-9]{4}-[A-Z0-9]{4}\b',log.read_text()) or [None])[0])
 s,_=request(ACCOUNTS+f'/v1/device/{code}/approve','POST',c['access_token'],{});check('Carbon approves CLI device flow',s in (200,204))
 check('CLI device flow completes',login.wait(timeout=30)==0);check('Carbon CLI reports identity',json.loads(cli('carbon-cli','login','status','--json').stdout)['uuid']==c['uuid'])
 device_log=OUT/'device.log'
 with device_log.open('w') as f:DEVICE=subprocess.Popen([str(BIN/'examples/fake_device'),API,'linux'],stdout=f,stderr=f,env={**os.environ,'FAKE_RECONNECT':'1'})
 code=wait(lambda:(re.search(r'PAIRING_CODE (\S+)',device_log.read_text()) or [None,None])[1])
 s,d=api('pairings',ct,'POST',{'pairing_code':code,'name':'Accounts rehearsal box','silicon_ids':[silicon['id']]},'pairing',{'Idempotency-Key':str(uuid.uuid4())});check('Native v1 enrollment pairs through Accounts v2',s==201);d=d['data'];did=d['device_id']
 wait(lambda:api('devices/'+did,ct)[1]['data']['online'])
 check('Carbon lists own device',any(v['device_id']==did for v in api('devices?scope=mine',ct)[1]['data']['items']))
 check('Unrelated Carbon cannot read private device',api('devices/'+did,ot)[0] in (403,404))
 check('Silicon CLI sees explicit grant',did in cli('silicon','device','ls','--json').stdout)
 sid=cli('silicon','session','new',did,'--connect').stdout.strip();check('Silicon starts device session',bool(sid))
 check('Native command relay', 'snapshot' in cli('silicon','snapshot','-i').stdout)
 check('Custodian sees its Silicon sessions',any(v['session_id']==sid for v in api('sessions?silicon='+silicon['id'],ct)[1]['data']['items']))
 check('Custodian cannot start as Silicon',api('sessions',ct,'POST',{'device_id':did},'session',{'Idempotency-Key':str(uuid.uuid4())})[0]==403)
 cli('silicon','screenshot','--ttl','2h','--out',str(OUT/'capture.png'));check('Screenshot transferred with real Accounts proofs',(OUT/'capture.png').stat().st_size>0)
 files=json.loads(cli('silicon','file','ls','--json').stdout)['items'];check('File shared with granting Carbon',bool(api('files?device_id='+did,ct)[1]['data']['items']))
 cli('silicon','file','keep',files[0]['file_id']);check('Keep file command',json.loads(cli('silicon','file','ls','--json').stdout)['items'][0]['permanent'])
 calls=[json.loads(l) for l in (ROOT/'.mig/dev-accounts/briefcase-calls.jsonl').read_text().splitlines()];check('Briefcase stand-in received delegated operations',len(calls)>0)
 # id changes arrive over the actual signed Accounts webhook.
 new_id=f'si:extend-e2e-renamed-{RUN}';s,_=request(ACCOUNTS+f'/v1/me/silicons/{silicon["uuid"]}/id','POST',c['access_token'],{'id':new_id},{'Idempotency-Key':str(uuid.uuid4())});check('Custodian renames Silicon',s==200)
 wait(lambda:new_id in cli('silicon','login','status','--json').stdout);check('Real webhook updates CLI identity',True)
 check('Forged webhook refused',request(API+'/webhooks/accounts','POST',body={'event_id':str(uuid.uuid4()),'type':'ping','data':{}},headers={'X-Accounts-Timestamp':str(int(time.time())),'X-Accounts-Signature':'v1=bad'})[0]==401)
 # Restart keeps Accounts keys and device credentials, and the native reconnect is accepted.
 subprocess.run([sys.executable,str(ROOT/'scripts/dev_accounts.py'),'restart'],check=True,stdout=subprocess.DEVNULL,timeout=45)
 check('Accounts session survives service restart',api('me',ct)[0]==200)
 check('CLI session survives service restart',json.loads(cli('silicon','login','status','--json').stdout)['authenticated'])
 check('Native app reconnects with existing device credential',wait(lambda:api('devices/'+did,ct)[1]['data']['online']))
 check('Grant removal',api('devices/'+did+'/access/'+new_id,ct,'DELETE')[0]==204)
 check('Revoking access ends the Silicon session',api('sessions/'+sid,ct)[1]['data']['state']=='ended')
 check('Revoked Silicon loses device access',did not in cli('silicon','device','ls','--json').stdout)
 cli('silicon','logout');check('CLI logout clears sign-in',json.loads(cli('silicon','login','status','--json').stdout)=={'authenticated':False})
 current=api('devices/'+did,ct)[1]['data'];s,_=api('devices/'+did,ct,'PATCH',{'name':'Renamed rehearsal box'},'device',{'If-Match':str(current['version'])});check('Device rename with current version',s==200)
 current=api('devices/'+did,ct)[1]['data'];check('Device removal',api('devices/'+did,ct,'DELETE',headers={'If-Match':str(current['version'])})[0]==204)
 COMPLETE=True
finally:
 if DEVICE:DEVICE.terminate();DEVICE.wait(timeout=10)
 (OUT/'results.json').write_text(json.dumps({'complete':COMPLETE,'checks':CHECKS},indent=2)+'\n');print(f'Complete: {COMPLETE}. {sum(x["passed"] for x in CHECKS)}/{len(CHECKS)} checks passed. Evidence {OUT}/results.json')
