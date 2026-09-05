#!/usr/bin/env python3
"""Capture the inlined signal-mode kernel from live J encoder execution.

At J#10736 instruction 4280, pitchcorr/average lag/harmonicity are in frame
+12/+8/+4, and local 19+24 is speech activity. Locals 20/32/30 point to
lags/F2/VUV state. Instruction 4584 follows the voiced decision. VUV layout
is voicing_prev, last_lag_prev, nrg_lo_bgn, nrg_hi_bgn (four f32).
All 220 original packet/PCM hashes must pass while these read-only markers run.
"""
import argparse
import json
import struct
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
SPANS=[('sig_input',4280,11,4,12),('sig_lags',4280,20,0,32),('sig_f2',4280,32,0,1028),
       ('sig_vuv_in',4280,30,0,16),('sig_spact',4280,19,24,4),('sig_cav',4280,5,52,4),
       ('sig_vuv_out',4584,30,0,16),('sig_result',4584,56,29136,4),('sig_voiced',4584,56,29160,4)]

def spec(out):
    s=json.loads((ROOT/'specs/mlow_110frames.json').read_text())
    s['functions']['core_encode']={'index_hint':10736}
    s['comment']=[__doc__]
    s['steps']=[{'op':'capture_memory','func':'core_encode','instruction':inst,'local':local,'at':at,'len':length,'count':330,'out':name} for name,inst,local,at,length in SPANS]+s['steps']
    out.write_text(json.dumps(s,indent=2)+'\n')

def assemble(run,out):
    records=[]
    def read(name,i,fmt):return list(struct.unpack('<'+fmt,(run/f'{name}_{i:04}.bin').read_bytes()))
    for i in range(330):
        harm,avg,pc=read('sig_input',i,'3f')
        records.append({'frame':i,'pitchcorr':pc,'avg_lag':avg,'harm':harm,
                        'lags':read('sig_lags',i,'8f'),'F2':read('sig_f2',i,'257f'),
                        'sp_act_prob':read('sig_spact',i,'f')[0],'cav':read('sig_cav',i,'i')[0],
                        'vuv_in':read('sig_vuv_in',i,'4f'),'vuv_out':read('sig_vuv_out',i,'4f'),
                        'vstr':read('sig_result',i,'f')[0],'voiced':read('sig_voiced',i,'i')[0]})
    out.write_text(json.dumps(records,separators=(',',':'))+'\n')

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['spec','assemble']);p.add_argument('output',type=Path);p.add_argument('--run',type=Path);a=p.parse_args()
    if a.action=='spec':spec(a.output)
    else:assemble(a.run,a.output)
