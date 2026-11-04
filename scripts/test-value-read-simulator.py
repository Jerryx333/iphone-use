#!/usr/bin/env python3
"""Complete-value conformance on an explicitly selected owned Simulator, no phone."""
import argparse, hashlib, importlib.util, json, os, plistlib, selectors, shutil, subprocess, time, urllib.request, urllib.error, urllib.parse
from pathlib import Path
ROOT=Path(__file__).resolve().parents[1]
BUNDLE='com.leeguoo.iphone-use.value-read-fixture'
spec=importlib.util.spec_from_file_location('value_transport',ROOT/'scripts/test-drag-simulator.py')
drag=importlib.util.module_from_spec(spec);spec.loader.exec_module(drag)

class Peer:
    def __init__(self,binary,base,owner,home,log):
        env={k:v for k,v in os.environ.items() if not k.startswith(('IPHONE_USE_','PHONE_REMOTE_','IPU_','TEST_RUNNER_'))}
        env.update(HOME=str(home),IPHONE_USE_URL=base,IPHONE_USE_OWNER=owner,IPHONE_USE_MCP_PREWARM='0',
            IPHONE_USE_FLOWS_NO_AUTO_UPDATE='1',IPHONE_USE_MCP_STRUCTURED='off',IPHONE_USE_MCP_TOOLS='readonly')
        with log.open('w') as out:self.process=subprocess.Popen([str(binary)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=out,env=env)
        self.selector=selectors.DefaultSelector();self.selector.register(self.process.stdout,selectors.EVENT_READ)
        self.buffer=b'';self.serial=0
        self.call('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'owned-value-conformance','version':'1'}})
        self.process.stdin.write(b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n');self.process.stdin.flush()
    def call(self,method,params):
        self.serial+=1;self.process.stdin.write((json.dumps({'jsonrpc':'2.0','id':self.serial,'method':method,'params':params})+'\n').encode());self.process.stdin.flush()
        deadline=time.monotonic()+30
        while time.monotonic()<deadline:
            while b'\n' in self.buffer:
                line,self.buffer=self.buffer.split(b'\n',1);reply=json.loads(line)
                if reply.get('id')==self.serial:return reply
            if self.selector.select(.2):
                part=os.read(self.process.stdout.fileno(),65536)
                if not part:raise RuntimeError('MCP exited before read reply')
                self.buffer+=part
        raise TimeoutError('readonly MCP request deadline; no write was sent')
    def close(self):
        self.process.terminate()
        try:self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:self.process.kill();self.process.wait(timeout=5)
        self.selector.close()

def main():
    p=argparse.ArgumentParser(description=__doc__)
    for name in ['base','udid','owner']:p.add_argument('--'+name,required=True)
    for name in ['mcp','runner-record','out']:p.add_argument('--'+name,required=True,type=Path)
    args=p.parse_args();args.out=args.out.resolve();args.out.mkdir(mode=0o700,parents=True,exist_ok=False)
    urllib.request.install_opener(urllib.request.build_opener(urllib.request.ProxyHandler({})))
    daemon=drag.Daemon(args.base,args.owner);before_status=daemon.require_simulator(args.udid)
    record=json.loads(args.runner_record.read_text())
    assert record['udid'].lower()==args.udid.lower() and record['daemon_port']==urllib.parse.urlsplit(args.base).port
    booted=json.loads(drag.command(['xcrun','simctl','list','devices','--json']).stdout)
    assert any(d['udid'].lower()==args.udid.lower() and d['state']=='Booted' for devices in booted['devices'].values() for d in devices)
    existing=drag.command(['xcrun','simctl','get_app_container',args.udid,BUNDLE,'data'],check=False)
    assert existing.returncode!=0,'owned fixture already exists; do not overwrite'
    fixture_root=args.out/'fixture-src';project=fixture_root/'runner/gesture-fixture'
    shutil.copytree(ROOT/'runner/gesture-fixture',project)
    (project/'main.swift').write_bytes((ROOT/'scripts/value-read-fixture.swift').read_bytes())
    pbx=project/'GestureFixture.xcodeproj/project.pbxproj'
    pbx.write_text(pbx.read_text().replace('com.leeguoo.iphone-use.gesture-fixture',BUNDLE))
    info=project/'Info.plist';metadata=plistlib.loads(info.read_bytes())
    metadata.update(CFBundleIdentifier=BUNDLE,CFBundleName='Value Read Fixture',CFBundleDisplayName='Value Read Fixture')
    metadata['CFBundleVersion']='2'
    metadata['UIApplicationSceneManifest']={'UIApplicationSupportsMultipleScenes':False,'UISceneConfigurations':{'UIWindowSceneSessionRoleApplication':[{'UISceneConfigurationName':'Value Fixture','UISceneDelegateClassName':'$(PRODUCT_MODULE_NAME).ValueSceneDelegate'}]}}
    info.write_bytes(plistlib.dumps(metadata))
    drag.ROOT=fixture_root;app=drag.build_fixture(args.out)
    built_info=plistlib.loads((app/'Info.plist').read_bytes())
    assert built_info['CFBundleIdentifier']==BUNDLE,'built fixture identity differs; nothing installed'
    scenes=built_info['UIApplicationSceneManifest']['UISceneConfigurations']['UIWindowSceneSessionRoleApplication']
    assert scenes[0]['UISceneDelegateClassName']=='GestureFixture.ValueSceneDelegate','unexpected fixture Scene delegate'

    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}));installed=False;peer=None
    result={'scope':'owned Simulator-only read conformance','input_sent':False,'cases':[]};cleanup=[]
    def api(payload):
        request=urllib.request.Request(args.base+'/agent/value',data=json.dumps(payload).encode(),headers={'Content-Type':'application/json','X-Phone-Owner':args.owner})
        try:
            with opener.open(request,timeout=20) as response:return response.status,json.load(response)
        except urllib.error.HTTPError as error:return error.code,json.load(error)
    try:
        drag.command(['xcrun','simctl','install',args.udid,app]);installed=True
        drag.command(['xcrun','simctl','launch',args.udid,BUNDLE])
        data=Path(drag.command(['xcrun','simctl','get_app_container',args.udid,BUNDLE,'data']).stdout.strip())
        receipt=data/'Documents/value-state.json';deadline=time.monotonic()+20
        while not receipt.exists() and time.monotonic()<deadline:time.sleep(.1)
        before=json.loads(receipt.read_text());assert before['events']==0 and not before['focused']
        assert before.get('fixture_variant')=='scene' and before.get('bundle_version')=='2'
        active,_=drag.runner_call(args,'/wda/activeAppInfo');assert active['value']['bundleId']==BUNDLE and active['value']['pid']==before['pid']
        expected='value-root-'+'字'*6015+'🙂';sha=hashlib.sha256(expected.encode()).hexdigest()
        assert before['sha256']==sha
        status,full=api({'identifier':'complete-field'})
        observed=full.get('value')
        result['full_read']={'http_status':status,'error':full.get('error'),'source':full.get('source'),
            'complete':full.get('complete'),'truncated':full.get('truncated'),
            'utf16_length':full.get('utf16_length'),'returned_utf16_length':full.get('returned_utf16_length'),
            'value_type':type(observed).__name__,
            'sha256':hashlib.sha256(observed.encode()).hexdigest() if isinstance(observed,str) else None,
            'expected_sha256':sha}
        if status!=200 or observed!=expected:
            ids,_=drag.runner_call(args,'/elements',{'using':'predicate string','value':"identifier == 'complete-field'"})
            element=ids['value'][0].get('ELEMENT') or ids['value'][0]['element-6066-11e4-a52e-4f735466cecf']
            try:native,_=drag.runner_call(args,f'/element/{element}/value?complete=1&max_utf16=8192')
            except urllib.error.HTTPError as error:native=json.load(error)
            value=native.get('value',{})
            result['native_diagnostic']={key:value.get(key) for key in ['error','message','source','complete','truncated','utf16_length','returned_utf16_length'] if isinstance(value,dict)}
        assert status==200 and full['value']==expected and full['complete'] is True and full['truncated'] is False, 'full AX value did not match App-owned seed'
        assert full['utf16_length']==len(expected.encode('utf-16-le'))//2
        result['cases'].append({'case':'full','sha256':sha,'utf16_length':full['utf16_length'],'passed':True})
        for limit,wanted in [(1,''),(2,'🙂')]:
            status,bounded=api({'identifier':'emoji-field','max_utf16':limit})
            assert status==200 and bounded['value']==wanted and bounded['truncated'] is True and not bounded['complete']
            result['cases'].append({'case':'scalar-boundary','limit_utf16':limit,'passed':True})
        status,quoted=api({'label':"Quote ' \\ 100%@ field\nsecond\tline"});assert status==200 and quoted['value']=='quoted-value'
        status,secure=api({'identifier':'secure-field'});assert status==403 and 'value' not in secure and 'OWNED_SYNTHETIC_SECRET' not in json.dumps(secure)
        status,ambiguous=api({'label':'Duplicate'});assert status==422 and ambiguous['error']=='ambiguous_element' and 'value' not in ambiguous
        status,missing=api({'identifier':'not-present'});assert status==422 and missing['error']=='element_not_found'
        status,flag=api({'identifier':'flag-field'});assert status==200 and flag['complete'] is True and flag['value'] in ['1','true']
        ids,_=drag.runner_call(args,'/elements',{'using':'predicate string','value':"identifier == 'emoji-field'"})
        element=ids['value'][0].get('ELEMENT') or ids['value'][0]['element-6066-11e4-a52e-4f735466cecf']
        legacy,_=drag.runner_call(args,f'/element/{element}/attribute/value');assert isinstance(legacy['value'],str),'snapshot attribute route changed'
        home=args.out/'mcp-home';home.mkdir(mode=0o700)
        peer=Peer(args.mcp.resolve(),args.base,args.owner,home,args.out/'mcp.log')
        reply=peer.call('tools/call',{'name':'phone_read_value','arguments':{'identifier':'complete-field'}})
        assert not reply.get('error') and reply['result'].get('isError') is not True
        texts=[item['text'] for item in reply['result']['content'] if item.get('type')=='text']
        observed=next(json.loads(text) for text in texts if text.startswith('{'))
        assert observed['value']==expected and sum(text.count(expected) for text in texts)==1
        assert 'value' not in reply['result'].get('structuredContent',{})
        result['cases'] += [{'case':name,'passed':True} for name in ['quoted-label','secure-refusal','ambiguity','missing','boolean','snapshot-attribute-scalar','real-stdio-single-copy']]
        after=json.loads(receipt.read_text());assert after==before,'read changed App state or focus'
        after_status=daemon.require_simulator(args.udid);assert after_status.get('owner')==before_status.get('owner'),'read changed owner lease'
        result.update(passed=True,app_state_unchanged=True,owner_unchanged=True)
    except Exception as error:
        result.update(passed=False,error=str(error));raise
    finally:
        if peer:peer.close()
        try:
            deadline=time.monotonic()+20
            while True:
                native,_=drag.runner_call(args,'/status')
                if native.get('value',{}).get('busy') is False:break
                if time.monotonic()>=deadline:raise RuntimeError('native read still pending; fixture cleanup deferred')
                time.sleep(.1)
            if installed:drag.command(['xcrun','simctl','uninstall',args.udid,BUNDLE])
        except Exception as error:cleanup.append(str(error))
        result['cleanup_errors']=cleanup
        (args.out/'result.json').write_text(json.dumps(result,indent=2)+'\n')
        print(json.dumps({k:v for k,v in result.items() if k!='cases'}),flush=True)
        if cleanup:raise RuntimeError('; '.join(cleanup))
if __name__=='__main__':main()
