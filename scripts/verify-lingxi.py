#!/usr/bin/env python3
"""Offline protocol checks against the real Lingxi plugin and a loopback upstream."""
import argparse, base64, collections, hashlib, http.server, json, os, platform, queue, subprocess, sys, tempfile, threading, time, urllib.request
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
PHONE='13800138000'
SEEN=[]
COUNTS=collections.Counter()
PROXY_SEEN=[]
STATE={'models':'normal','balance':'normal','captcha':False,'claim':'normal','identity':PHONE,'refresh':'normal','sms_ack':'null','sms_code':0,'sms_http':200}
PNG='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j/pkAAAAASUVORK5CYII='
class Handler(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_POST(self):
  body=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))))
  path=self.path; SEEN.append((path,dict(self.headers),body));COUNTS[path]+=1
  token=self.headers.get('Authorization','')
  assert self.headers.get('X-Client-Id')=='lingxiclaw'
  code=0; status=200; data={}; omit_data=False
  if path.endswith('getUserInfo'):
   if 'expired' in token: status=401
   else: data={'phone':STATE['identity'],'id':12}
  elif path.endswith('agentRefreshToken'):
   assert body['refresh_token']=='fixture-refresh'
   if STATE['refresh']=='fail':code=1018
   elif STATE['refresh']=='null':data=None
   else:data={'access_token':'fixture-new','access_token_expires_at':int(time.time())+3600}
  elif path.endswith('getTokenBalance'):
   if 'expired' in token: code=3004
   elif STATE['balance']=='missing':data={}
   elif STATE['balance']=='invalid':data={'compute_credit':'NaN'}
   else:data={'compute_credit':'128.5'}
  elif path.endswith('getAvailableModels'):
   if STATE['models']=='empty':data=[]
   elif STATE['models']=='bad':data=[{'id':52}]
   else:data={'items':[{'id':52,'modelName':'ydy-fixture','name':'Fixture','extInfo':{'supports_images':True,'context_length':131072}},{'id':53,'modelName':'ydy-fixture','name':'Duplicate'}]}
  elif path.endswith('/chat/completions'):
   assert token.startswith('Bearer fixture-') and body['model']=='ydy-fixture'
   if body['stream']:
    raw=('data: '+json.dumps({'choices':[{'delta':{'reasoning_content':'推理','content':'你好','tool_calls':[{'index':0,'id':'call-1','function':{'name':'echo','arguments':'{}'}}]}}]},ensure_ascii=False)+'\n\ndata: [DONE]\n\n').encode()
    self.send_response(200);self.send_header('Content-Type','text/event-stream');self.end_headers();self.wfile.write(raw);return
   data={'choices':[{'message':{'content':'你好'}}],'usage':{'completion_tokens':3}}
   raw=json.dumps(data,ensure_ascii=False).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(raw);return
  elif path.endswith('marketing-campaigns'):
   data={'items':[{'campaign_id':7,'activity_name':'AI每日畅想计划','claimable':True,'status':1,'claim_status':'','eligible':True},{'campaign_id':8,'activity_name':'Unrelated','claimable':True,'status':1}]}
  elif path.endswith('/claim'):
   assert body=={'campaign_id':7}
   if STATE['claim']=='unknown': status=500
   else:data={'status':'SUCCESS','message':'领取成功'}
  elif path.endswith('agentSendCaptcha'):data={'captcha_id':'cap-1','captcha_image':PNG}
  elif path.endswith('agentSendSmsCode'):
   if STATE['captcha'] and body.get('captcha_code')!='ABCD':code=1012
   elif STATE['captcha']:assert body['captcha_id']=='cap-1'
   if code==0:code=STATE['sms_code']
   status=STATE['sms_http']
   data=None if STATE['sms_ack']=='null' else {}
   omit_data=STATE['sms_ack']=='missing'
  elif path.endswith('agentSmsLogin'):
   assert body['platform']=='web' and body['agent_code']=='lingxiclaw' and body['device_uid']=='default'
   if body['sms_code']=='111111':data=None
   elif body['sms_code']!='123456':code=1014
   else:data={'access_token':'fixture-sms','refresh_token':'fixture-refresh','user':{'phone':PHONE},'access_token_expires_at':int(time.time())+3600}
  else:raise AssertionError(path)
  envelope={'code':code,'message':'private-upstream-text'}
  if not omit_data:envelope['data']=data
  raw=json.dumps(envelope,ensure_ascii=False).encode()
  self.send_response(status);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(raw)
class ProxyHandler(Handler):
 def do_POST(self):
  PROXY_SEEN.append(self.path)
  super().do_POST()
class RPC:
 def __init__(self,exe,data_dir,base):
  self.process=subprocess.Popen([str(exe)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,bufsize=1,env={**os.environ,'LINGXI_TEST_BASE':base})
  self.frames=queue.Queue();self.store={};self.ledger={};self.next_id=0;self.updates=0;self.reject_next=False;self.data_dir=data_dir
  def reader():
   for line in self.process.stdout:self.frames.put(json.loads(line))
  threading.Thread(target=reader,daemon=True).start()
 def write(self,frame):self.process.stdin.write(json.dumps(frame)+'\n');self.process.stdin.flush()
 def call(self,method,params=None,error=False):
  self.next_id+=1;rid=self.next_id
  self.write({'id':rid,'method':method,'params':params if params is not None else {'provider':'lingxi','account':'one'}})
  while True:
   f=self.frames.get(timeout=45)
   if 'method' in f:
    p=f.get('params',{});m=f['method']
    if m=='store.get':result=self.store[p['account']]
    elif m=='store.list':result=[v for v in self.store.values() if v['provider']==p['provider']]
    elif m=='store.update':
     self.updates+=1
     current=self.store[p['account']]
     if self.reject_next:
      self.reject_next=False;current['secret']={**current['secret'],'access_token':'fixture-other-concurrent'};result={'updated':False}
     elif current['secret']==p['expect']:current['secret']=p['secret'];result={'updated':True}
     else:result={'updated':False}
    elif m=='store.ledger_get':result=self.ledger.get((p['account'],p['kind']),None)
    elif m=='store.ledger_put':self.ledger[p['account'],p['kind']]=p['value'];result=None
    elif m=='notify':result=None
    else:raise AssertionError(f)
    self.write({'id':f['id'],'result':result})
   elif f.get('id')==rid and ('result'in f or 'error'in f):
    if error:assert 'error'in f,f;return f['error']
    assert 'error'not in f,f
    return f['result']
 def init(self,proxy='direct'):
  return self.call('init',{'env':{'data_dir':self.data_dir,'host_version':'offline-fixture','target':'aarch64-apple-darwin' if sys.platform=='darwin' else 'x86_64-unknown-linux-gnu','proxy':proxy,'debug_traffic':False}})
 def close(self):
  if self.process.poll() is None:self.process.terminate();self.process.wait(timeout=5)
def main():
 ap=argparse.ArgumentParser();ap.add_argument('--exe',type=Path,default=ROOT/'target/debug/agent2api-lingxi');args=ap.parse_args()
 server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler);base='http://127.0.0.1:'+str(server.server_port)
 threading.Thread(target=server.serve_forever,daemon=True).start();passed=[]
 def check(label,cond=True):assert cond,label;passed.append(label);print('PASS',label,flush=True)
 with tempfile.TemporaryDirectory(prefix='lingxi-offline-') as temp:
  rpc=RPC(args.exe,temp,base)
  try:
   meta=rpc.init()[0];check('truthful handshake',meta['id']=='lingxi' and meta['capabilities']['refresh'] and meta['capabilities']['checkin'])
   methods=rpc.call('admin.action',{'provider':'lingxi','action':'auth.methods'})['methods'];check('masked dynamic forms',methods[0]['prompts'][0]['secret'] and methods[1]['methodIndex']==1)
   payload={'methodIndex':0,'inputs':{'access_token':'Bearer fixture-access','refresh_token':'fixture-refresh'}}
   def normalize(p):return rpc.call('account.normalize',{'provider':'lingxi','source':{'kind':'manual','payload':p},'name':'测试账户'})
   norm=normalize(payload);check('manual verifies actual cloud identity',norm['identity']=='lingxi:'+hashlib.sha256(PHONE.encode()).hexdigest())
   check('public view omits secrets and full phone',all(s not in json.dumps(norm['public']) for s in ['fixture-access','fixture-refresh',PHONE]))
   check('SMS code never persisted','sms_code'not in norm['secret'])
   for source in [{'kind':'file_text','content':'fixture-access'},{'kind':'file_base64','content':base64.b64encode(json.dumps(payload['inputs']).encode()).decode()}]:
    out=rpc.call('account.normalize',{'provider':'lingxi','source':source});check('file import '+source['kind'],out['identity']==norm['identity'])
   for label,p in [('API key',{'access_token':'lx_workbench-key'}),('header injection',{'access_token':'bad\r\nheader'}),('method',{'methodIndex':4,'inputs':{}}),('field type',{'methodIndex':0,'inputs':{'access_token':True}}),('unknown field',{'methodIndex':0,'inputs':{'access_token':'fixture-access','x':'private'}})]:
    result=rpc.call('account.normalize',{'provider':'lingxi','source':{'kind':'manual','payload':p}},error=True);check('reject '+label,bool(result) and 'private-upstream-text'not in str(result))
   def save(id='one',secret=None):rpc.store[id]={'id':id,'provider':'lingxi','secret':secret or json.loads(json.dumps(norm['secret'])),'public':norm['public'],'proxy':{'state':'unset'}}
   save();check('token ensure uses account store',rpc.call('token.ensure')=='fixture-access')
   models=rpc.call('models.list')['models'];check('modelName is wire ID and duplicates removed',len(models)==1 and models[0]['id']=='ydy-fixture' and models[0]['upstream_id']==52)
   check('model facts retained',models[0]['extInfo']['supports_images'] and models[0]['contextWindow']==131072)
   for mode in ['bad','empty']:
    STATE['models']=mode;check('reject '+mode+' catalog',bool(rpc.call('models.list',error=True)))
   STATE['models']='normal';check('refresh models',rpc.call('models.refresh',{'provider':'lingxi','account':'one','force':True}) is not None)
   body={'model':'alias','messages':[{'role':'user','content':'你好'}],'tools':[{'type':'function','function':{'name':'echo','parameters':{'type':'object'}}}],'temperature':0.4,'reasoning_effort':'high','max_tokens':32,'stream_options':{'include_usage':True}}
   params={'provider':'lingxi','account':'one','body':body,'headers':{},'wire_model':'ydy-fixture','stream':True,'reasoning':''}
   opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
   for stream in [True,False]:
    plan=rpc.call('chat',{**params,'stream':stream})
    check('chat fields and tools retained '+str(stream),plan['body']=={**body,'model':'ydy-fixture','stream':stream})
    response=opener.open(urllib.request.Request(plan['url'],data=json.dumps(plan['body']).encode(),headers=dict(plan['headers'])),timeout=5).read().decode()
    check('SSE reasoning tools and DONE' if stream else 'nonstream usage',all(v in response for v in ['你好','reasoning_content','tool_calls','[DONE]']) if stream else json.loads(response)['usage']['completion_tokens']==3)
   usage=rpc.call('usage.query');check('verified balance only',usage['balance']==128.5 and 'limit'not in usage['windows'][0])
   for mode in ['missing','invalid']:
    STATE['balance']=mode;check('reject '+mode+' balance',bool(rpc.call('usage.query',error=True)))
   STATE['balance']='normal'
   rpc.store['one']['secret']['access_token']='fixture-expired';check('auth failure refreshes and replays read once',rpc.call('usage.query')['balance']==128.5 and rpc.store['one']['secret']['access_token']=='fixture-new')
   check('refresh token preserved when omitted',rpc.store['one']['secret']['refresh_token']=='fixture-refresh')
   rpc.store['one']['secret']['access_token_expires_at']=int(time.time())-1;check('proactive expiry refresh',rpc.call('token.ensure')=='fixture-new')
   rpc.reject_next=True;check('CAS preserves concurrent replacement',rpc.call('token.refresh')=='fixture-other-concurrent')
   STATE['identity']='13800138001';before=json.loads(json.dumps(rpc.store['one']['secret']));check('refresh rejects identity change',bool(rpc.call('token.refresh',error=True)) and rpc.store['one']['secret']==before);STATE['identity']=PHONE
   save('two');rpc.store['two']['secret']['access_token']='fixture-second';check('account isolation',rpc.call('token.ensure',{'provider':'lingxi','account':'two'})=='fixture-second')
   rpc.store['one']['proxy']={'state':'failed','message':'private-proxy'};check('failed proxy does not bypass',bool(rpc.call('usage.query',error=True)));rpc.store['one']['proxy']={'state':'unset'}
   plan=rpc.call('chat',{**params,'proxy':{'protocol':'http','host':'127.0.0.1','port':1}});check('chat accepts explicit proxy without remote call',plan['url'].startswith(base))
   for status,kind in [(401,'token_expired'),(402,'quota_limited'),(429,'quota_limited'),(500,'fatal')]:
    result=rpc.call('classify',{'provider':'lingxi','status':status,'body':{}});check('classification '+str(status),result.get('kind',result.get('type'))==kind)
   first=rpc.call('checkin.claim');check('daily activity claimed',first['outcome']=='claimed')
   count=COUNTS['/ai/llm/order-center/public/client/v1/marketing-campaigns/claim'];check('claim ledger prevents duplicate',rpc.call('checkin.claim')['outcome']=='done' and COUNTS['/ai/llm/order-center/public/client/v1/marketing-campaigns/claim']==count)
   rpc.ledger.clear();STATE['claim']='unknown';check('unknown claim reported failed',rpc.call('checkin.claim')['outcome']=='failed');count=COUNTS['/ai/llm/order-center/public/client/v1/marketing-campaigns/claim'];rpc.call('checkin.claim');check('unknown write never replayed',COUNTS['/ai/llm/order-center/public/client/v1/marketing-campaigns/claim']==count)
   def start(p=PHONE):return rpc.call('login.start',{'provider':'lingxi','method':'browser','payload':{'methodIndex':1,'inputs':{'phone':p}}})['flow']
   def command(flow,**fields):return rpc.call('login.callback',{'provider':'lingxi','flow':flow,'code':json.dumps(fields),'url':''})
   count=len(SEEN);flow=start();check('starting login sends no SMS or upstream call',len(SEEN)==count)
   poll=rpc.call('login.poll',{'provider':'lingxi','flow':flow});check('awaiting user action',poll['state']=='pending')
   poll=command(flow,action='captcha');hint=json.loads(poll['hint']);check('captcha rendered as data image only',hint['status']=='captcha' and hint['captcha_image']==PNG)
   STATE['captcha']=True
   check('captcha required before sending',json.loads(command(flow,action='send')['hint'])['status']=='captcha_required')
   check('null SMS acknowledgement is accepted and transmits captcha',json.loads(command(flow,action='send',captcha_code='ABCD')['hint'])['status']=='sent')
   check('phone cooldown across requests',json.loads(command(flow,action='send',captcha_code='ABCD')['hint'])['status']=='rate_limited')
   check('invalid SMS can be corrected',json.loads(command(flow,action='verify',sms_code='000000',captcha_code='ABCD')['hint'])['status']=='invalid_code')
   done=command(flow,action='verify',sms_code='123456',captcha_code='ABCD',name='短信账户');check('SMS confirmation returns normalized account',done['state']=='done' and done['account']['identity']==norm['identity'])
   check('SMS saved credentials only',done['account']['secret']['access_token']=='fixture-sms' and 'sms_code'not in done['account']['secret'])
   check('SMS public response excludes credentials',all(s not in json.dumps(done['account']['public']) for s in [PHONE,'fixture-sms','fixture-refresh','123456']))
   check('completed login cannot replay',command(flow,action='verify',sms_code='123456')['state']=='failed')
   flow=start('13800138001');rpc.call('login.cancel',{'provider':'lingxi','flow':flow});check('cancel stops callbacks',command(flow,action='send')['state']=='failed')
   STATE['captcha']=False
   for i,(shape,code) in enumerate([('null',200),('missing',0),('missing',200),('object',0)]):
    STATE['sms_ack']=shape;STATE['sms_code']=code;f=start(f'138001380{10+i}')
    result=command(f,action='send');check(f'SMS ack {shape} code {code}',json.loads(result['hint'])['status']=='sent')
    check('successful ack stays ready for verification '+str(i),json.loads(rpc.call('login.poll',{'provider':'lingxi','flow':f})['hint'])['status']=='sent')
    rpc.call('login.cancel',{'provider':'lingxi','flow':f})
   STATE['sms_ack']='null';STATE['sms_code']=1012
   f=start('13800138020');result=command(f,action='send');check('null data does not hide business failure',json.loads(result['hint'])['status']=='captcha_required');rpc.call('login.cancel',{'provider':'lingxi','flow':f})
   STATE['sms_code']=0;STATE['sms_http']=500
   f=start('13800138021');result=command(f,action='send');check('successful code does not hide HTTP failure',json.loads(result['hint'])['status']=='error');rpc.call('login.cancel',{'provider':'lingxi','flow':f})
   STATE['sms_http']=200
   f=start('13800138022');command(f,action='send');result=command(f,action='verify',sms_code='111111')
   check('null login data never creates account',result['state']=='pending' and json.loads(result['hint'])['status']=='error' and 'account'not in result)
   rpc.call('login.cancel',{'provider':'lingxi','flow':f})
   STATE['refresh']='null';check('null refresh data remains an error',bool(rpc.call('token.refresh',error=True)));STATE['refresh']='normal'

   # Use an unreachable gateway default to prove explicit direct/account proxy
   # overrides it. The proxy fixture records absolute-form requests separately.
   proxy_server=http.server.ThreadingHTTPServer(('127.0.0.1',0),ProxyHandler)
   threading.Thread(target=proxy_server.serve_forever,daemon=True).start()
   proxy_rpc=RPC(args.exe,temp,base)
   try:
    proxy_rpc.init('http://127.0.0.1:1')
    proxy_rpc.store['one']={'id':'one','provider':'lingxi','secret':norm['secret'],'public':norm['public'],'proxy':{'state':'static','config':{'source':'direct'}}}
    check('static direct fetches models despite gateway proxy',len(proxy_rpc.call('models.list')['models'])==1)
    check('static direct queries balance despite gateway proxy',proxy_rpc.call('usage.query')['balance']==128.5)
    check('static direct refreshes tokens',proxy_rpc.call('token.refresh')=='fixture-new')
    proxy_url='http://127.0.0.1:'+str(proxy_server.server_port)
    for key in ['url','proxy','address','server']:
     proxy_rpc.store['one']['proxy']={'state':'static','config':{key:proxy_url}}
     count=len(PROXY_SEEN)
     check('static '+key+' actually routes through account proxy',proxy_rpc.call('usage.query')['balance']==128.5 and len(PROXY_SEEN)==count+1)
    proxy_rpc.store['one']['proxy']={'state':'static','config':{'config':{'source':'custom','protocol':'http','host':'127.0.0.1','port':str(proxy_server.server_port)}}}
    count=len(PROXY_SEEN)
    check('nested static host config routes models through proxy',len(proxy_rpc.call('models.list')['models'])==1 and len(PROXY_SEEN)==count+1)
    proxy_rpc.store['one']['proxy']={'state':'resolved','proxy':{'protocol':'http','host':'127.0.0.1','port':proxy_server.server_port}}
    count=len(PROXY_SEEN)
    check('gateway resolved proxy remains supported',proxy_rpc.call('usage.query')['balance']==128.5 and len(PROXY_SEEN)==count+1)
    for source in ['pool','rotate','clash','system']:
     proxy_rpc.store['one']['proxy']={'state':'static','config':{'source':source,'url':base}}
     count=len(SEEN)
     check('unresolved '+source+' never bypasses selection',bool(proxy_rpc.call('usage.query',error=True)) and len(SEEN)==count)
    for config in [{'source':'custom','host':'127.0.0.1','port':0},{'url':'http://fixture-user:fixture-password@127.0.0.1:99999'},{'url':'file:///tmp/proxy'},{'source':'custom'}]:
     proxy_rpc.store['one']['proxy']={'state':'static','config':config}
     count=len(SEEN);err=proxy_rpc.call('usage.query',error=True)
     check('invalid static proxy fails without exposing credentials',bool(err) and len(SEEN)==count and 'fixture-password'not in str(err))
    proxy_rpc.store['one']['proxy']={'state':'failed','message':'fixture-password'}
    count=len(SEEN);check('failed account proxy never falls back',bool(proxy_rpc.call('usage.query',error=True)) and len(SEEN)==count)
    proxy_rpc.store['one']['proxy']={'state':'unset'}
    count=len(SEEN);check('unset account follows gateway default',bool(proxy_rpc.call('usage.query',error=True)) and len(SEEN)==count)
    proxy_rpc.call('shutdown',{});proxy_rpc.process.wait(timeout=5)
   finally:
    proxy_rpc.close();proxy_server.shutdown()

   rpc.call('shutdown',{});rpc.process.wait(timeout=5);check('graceful shutdown',rpc.process.returncode==0)
   check('stderr contains no credentials',all(s not in rpc.process.stderr.read() for s in ['fixture-access','fixture-refresh','123456',PHONE]))
  finally:rpc.close()
 server.shutdown();print(f'{len(passed)} checks passed; no live account, SMS or credits used.')
if __name__=='__main__':main()
