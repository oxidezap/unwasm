#!/usr/bin/env python3
"""Read-only traces of pitch search and LSF quantization in the live encoder.

Pitch is inlined in J#10736. Entry at op1691 has the filtered 659-float LTP
buffer at local47 and PitchEstimator at local22; op4266 is after selection.
LSF core J#10804 has the original 13-argument ABI. Params8..12 retain output
pointers until its implicit return. Conditional centroid state is p7 (nullable).
"""
import argparse
import json
import struct
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
PITCH=[('pitch_ltp',1691,47,0,2636),('pitch_state',1691,22,4,32),('pitch_f2',1691,32,0,1028),
       ('pitch_cav',1691,5,52,4),('pitch_output',4266,11,4,12),('pitch_lags',4266,20,0,32),
       ('pitch_indices',4266,68,0,32),('pitch_block',4266,69,0,4)]

def spec(out,lsf_count,end):
    s=json.loads((ROOT/'specs/mlow_110frames.json').read_text());s['comment']=[__doc__]
    s['functions']['core_encode']={'index_hint':10736};s['functions']['lsf_core']={'index_hint':10804}
    spans=[{'op':'capture_memory','func':'core_encode','instruction':inst,'local':local,'at':at,'len':length,'count':330,'out':name} for name,inst,local,at,length in PITCH]
    for name,inst,local,length in [('lsf_a',0,3,68),('lsf_cond',0,7,2176),('lsf_q',end,8,64),('lsf_qi',end,9,17),('lsf_bits',end,10,4),('lsf_rd',end,11,4),('lsf_weights',end,12,64)]:
        spans.append({'op':'capture_memory','func':'lsf_core','instruction':inst,'local':local,'len':length,'count':lsf_count,'out':name})
    for name,local,fp in [('lsf_surv',2,False),('lsf_rdw',4,True),('lsf_voiced',5,False),('lsf_lowrate',6,False),('lsf_cond_ptr',7,False)]:
        spans.append({'op':'capture_value','func':'lsf_core','instruction':0,'local':local,'float':fp,'count':lsf_count,'out':name})
    s['steps']=spans+s['steps'];out.write_text(json.dumps(s,indent=2)+'\n')

def assemble(run,pitch_out,lsf_out,count):
    def rd(name,i,fmt):return list(struct.unpack('<'+fmt,(run/f'{name}_{i:04}.bin').read_bytes()))
    records=[]
    for i in range(330):
        prev=rd('pitch_state',i,'2f6i');harm,avg,pc=rd('pitch_output',i,'3f')
        records.append({'frame':i,'prev_lag':prev[0],'prev_pitch_corr':prev[1],'prev_lagblk':prev[2],'prev_lagidx':prev[3],'numstates':prev[5],'low_rate':prev[6],'low_complexity':prev[7],
                        'ltp_buf':rd('pitch_ltp',i,'659f'),'F2':rd('pitch_f2',i,'257f'),'cav':rd('pitch_cav',i,'i')[0],
                        'pitchcorr':pc,'avg_lag':avg,'harm':harm,'lags':rd('pitch_lags',i,'8f'),'laginds':rd('pitch_indices',i,'8i'),'blockseg_idx':rd('pitch_block',i,'i')[0]})
    pitch_out.write_text(json.dumps(records,separators=(',',':'))+'\n')
    records=[]
    for i in range(count):
        r={'A':rd('lsf_a',i,'17f'),'qlsf':rd('lsf_q',i,'16f'),'qi':rd('lsf_qi',i,'17b'),'bits':rd('lsf_bits',i,'f')[0],
           'RDbest':rd('lsf_rd',i,'f')[0],'weights':rd('lsf_weights',i,'16f'),'surv':rd('lsf_surv',i,'i')[0],
           'RDw_adj':rd('lsf_rdw',i,'f')[0],'voiced':rd('lsf_voiced',i,'i')[0],'lowRate':rd('lsf_lowrate',i,'i')[0],
           'cond':rd('lsf_cond',i,'544f') if rd('lsf_cond_ptr',i,'I')[0] else None}
        records.append(r)
    lsf_out.write_text(json.dumps(records,separators=(',',':'))+'\n')

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['spec','assemble']);p.add_argument('output',type=Path);p.add_argument('--lsf-count',type=int,required=True);p.add_argument('--end',type=int);p.add_argument('--run',type=Path);p.add_argument('--lsf-output',type=Path);a=p.parse_args()
    if a.action=='spec':spec(a.output,a.lsf_count,a.end)
    else:assemble(a.run,a.output,a.lsf_output,a.lsf_count)
