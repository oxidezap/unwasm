#!/usr/bin/env python3
"""Re-derive the pinned MLOW corpus and refuse any output/selector drift.

Run from any directory. --update-lock records an intentional re-derivation;
CI never uses it. All temporary work lives under --out and can be deleted.
"""
import argparse
import hashlib
import importlib.util
import json
import os
import struct
import subprocess
from pathlib import Path
import capture_specs, fe_trace, signal_trace, kernel_trace, postfilter_trace, params_trace, gennoise_trace

ROOT=Path(__file__).resolve().parents[2]
LOCK=ROOT/'specs/mlow.lock.json'
RUNS={
 'JgwtTQVeWPm':['mlow_110frames','mlow_120ms','mlow_dtx_off','mlow_fe_trace','mlow_signal_trace',
                'mlow_kernel_trace','mlow_postfilter_trace','mlow_params_trace','mlow_gennoise_trace'],
 'S_ivh1PriOA':['mlow_110frames_s','mlow_120ms_s'],
}

def digest(data):return hashlib.sha256(data).hexdigest()

def tree(manifest,run):
 h=hashlib.sha256()
 for r in sorted(manifest['outputs'],key=lambda r:r['file']):
  payload=(run/r['file']).read_bytes()
  assert len(payload)==r['bytes'] and digest(payload)==r['sha256'],f"corrupt output {r['file']}"
  name=r['file'].encode();h.update(struct.pack('<I',len(name)));h.update(name);h.update(struct.pack('<Q',len(payload)));h.update(bytes.fromhex(r['sha256']))
 return h.hexdigest()

def regenerate_specs(directory):
 directory.mkdir(parents=True,exist_ok=True)
 capture_specs.main(directory)
 for module,stem in [(fe_trace,'mlow_fe_trace'),(signal_trace,'mlow_signal_trace'),(postfilter_trace,'mlow_postfilter_trace'),(params_trace,'mlow_params_trace'),(gennoise_trace,'mlow_gennoise_trace')]:
  module.spec(directory/(stem+'.json'))
 kernel_trace.spec(directory/'mlow_kernel_trace.json',330,1099)
 for p in directory.glob('*.json'):
  assert p.read_bytes()==(ROOT/'specs'/p.name).read_bytes(),f"generated spec drift: {p.name}"

def fetch(captures):
 spec=importlib.util.spec_from_file_location('fetch_wasm',ROOT/'scripts/fetch-wasm.py')
 module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
 pinned=json.loads((ROOT/'wasm.lock.json').read_text())['modules']
 dest=ROOT/'wasm';dest.mkdir(exist_ok=True)
 wanted={p['fileName']:p for p in pinned if p['fileName'].removesuffix('.wasm') in captures}
 assert len(wanted)==len(captures),'capture not in wasm.lock.json'
 missing={name:p for name,p in wanted.items() if not module.satisfied(dest,p)}
 if missing:module.take_from_origin(missing,dest)
 if missing:
  # Existing downloader also knows the archived release fallback and token routing.
  subprocess.run(['python3',str(ROOT/'scripts/fetch-wasm.py'),str(dest)],check=True,cwd=ROOT)
 for p in wanted.values():assert module.satisfied(dest,p),f"capture unavailable: {p['fileName']}"

def assemble(out):
 dest=out/'artifacts';dest.mkdir(exist_ok=True)
 run=out/'mlow_110frames'
 frames=[(run/f'packet{i:03}.bin').read_bytes() for i in range(110)]
 (dest/'wasm_derived_frames.json').write_text(json.dumps([p.hex() for p in frames],indent=2)+'\n')
 (dest/'wasm_derived_ref.raw').write_bytes(b''.join((run/f'decoded{i:03}.raw').read_bytes() for i in range(110)))
 (dest/'wasm_derived_vad.json').write_text(json.dumps([{'frame':i,'toc':p[0],'cav':int(p[0]!=0x10),'len':len(p)} for i,p in enumerate(frames)],indent=2)+'\n')
 run=out/'mlow_120ms'
 (dest/'wasm_derived_120ms_frames.json').write_text(json.dumps([(run/f'pkt120_{i}.bin').read_bytes().hex() for i in range(8)],indent=2)+'\n')
 (dest/'wasm_derived_120ms_ref.raw').write_bytes(b''.join((run/f'dec120_{i}.raw').read_bytes() for i in range(8)))
 fe_trace.assemble(out/'mlow_fe_trace',dest/'wasm_fe.json')
 signal_trace.assemble(out/'mlow_signal_trace',dest/'wasm_signal_mode.json')
 kernel_trace.assemble(out/'mlow_kernel_trace',dest/'wasm_pitch.json',dest/'wasm_lsf_quant.json',330)
 postfilter_trace.assemble(out/'mlow_postfilter_trace',dest/'wasm_hp_postfilter.json',dest/'wasm_harm_postfilter.json')
 params_trace.assemble(out/'mlow_params_trace',dest/'wasm_params.json')
 gennoise_trace.assemble(out/'mlow_gennoise_trace',dest/'wasm_gennoise.json')


def main():
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('--capture',choices=['all',*RUNS],default='all');p.add_argument('--out',type=Path,default=ROOT/'.derive-mlow');p.add_argument('--oracle',type=Path,default=ROOT/'target/release/oracle');p.add_argument('--update-lock',action='store_true');a=p.parse_args()
 captures=list(RUNS) if a.capture=='all' else [a.capture]
 assert not a.update_lock or a.capture=='all','lock refresh requires all captures'
 out=a.out.resolve();out.mkdir(parents=True,exist_ok=True);oracle=a.oracle.resolve()
 regenerate_specs(out/'generated');fetch(captures)
 expected=json.loads(LOCK.read_text()) if LOCK.exists() else {}
 actual={'schema':1,'inputs':{'synth_mic.raw':digest((ROOT/'specs/synth_mic.raw').read_bytes()),'synth120_head.raw':digest((ROOT/'specs/synth120_head.raw').read_bytes())},'runs':{}}
 if not a.update_lock:assert actual['inputs']==expected.get('inputs'),'input corpus drift'
 for capture in captures:
  for name in RUNS[capture]:
   run=out/name;run.mkdir(exist_ok=True)
   with (out/(name+'.log')).open('w') as log:
    result=subprocess.run([str(oracle),'derive','--spec',str(ROOT/'specs'/(name+'.json')),'--out',str(run)],cwd=ROOT,stdout=log,stderr=subprocess.STDOUT)
   if result.returncode:
    raise RuntimeError((out/(name+'.log')).read_text()[-8000:])
   manifest=json.loads((run/'manifest.json').read_text())
   record={'module':manifest['module'],'spec_sha256':manifest['spec_sha256'],'resolutions':manifest['resolutions'],
           'outputs':len(manifest['outputs']),'tree_sha256':tree(manifest,run)}
   actual['runs'][name]=record
   if not a.update_lock:assert record==expected['runs'][name],f'derivation drift: {name}'
   print(f'{capture}: {name}: {record["outputs"]} outputs verified',flush=True)
 if 'JgwtTQVeWPm' in captures:assemble(out)
 if a.update_lock:LOCK.write_text(json.dumps(actual,indent=2)+'\n')
 print('MLOW derivation verified',flush=True)
if __name__=='__main__':main()
