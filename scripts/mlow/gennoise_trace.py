#!/usr/bin/env python3
"""Derive live decoder excitation/noise cases from J#10726 op2631/2632.

These snapshots replace replay inputs with inputs computed by the shipped
encoder and decoder from synth_mic.raw. Observation preserves all 220 hashes.
"""
import argparse,json,struct
from pathlib import Path
def ng_value(data):
 values=list(struct.unpack('<11f3i',data));result={}
 for name,count in [('env_smth',1),('env_last',1),('out_state_uv',2),('out_state_v',2),('corr_smth',3),('shape_state',2)]:
  result[name]=values[0] if count==1 else values[:count];del values[:count]
 result.update(zip(['prev_voiced','since_unvoiced','rand_seed'],values));return result
ROOT=Path(__file__).resolve().parents[2]
SPANS=[('noise_in',2631,93,0,56),('noise_out',2632,93,0,56),('noise_exc',2631,25,0,320),
       ('noise_audio',2632,11,128,320),('noise_lsfs',2631,11,2208,256),('noise_lags',2631,11,4160,96),
       ('noise_voiced',2631,11,2744,4),('noise_fcbg',2631,8,4,2),('noise_pulses',2631,9,0,2)]

def spec(out):
 s=json.loads((ROOT/'specs/mlow_110frames.json').read_text());s['functions']['core_decode']={'index_hint':10726};s['comment']=[__doc__]
 captures=[{'op':'capture_memory','func':'core_decode','instruction':inst,'local':local,'at':at,'len':length,'count':1320,'out':name} for name,inst,local,at,length in SPANS]
 for name,local,fp in [('noise_nrg',101,True),('noise_nbr',109,True),('noise_sf',10,False),('noise_frame',32,False),('noise_len',18,False)]:captures.append({'op':'capture_value','func':'core_decode','instruction':2631,'local':local,'float':fp,'count':1320,'out':name})
 s['steps']=captures+s['steps'];out.write_text(json.dumps(s,indent=2)+'\n')

def assemble(run,out):
 def raw(name,i):return (run/f'{name}_{i:04}.bin').read_bytes()
 def rd(name,i,fmt):return list(struct.unpack('<'+fmt,raw(name,i)))
 records=[]
 for i in range(1320):
  sf=rd('noise_sf',i,'i')[0];frame=rd('noise_frame',i,'i')[0]
  assert sf==i%4 and frame==i//4%3 and rd('noise_len',i,'i')[0]==80
  ngin=ng_value(raw('noise_in',i));ngout=ng_value(raw('noise_out',i));exc=rd('noise_exc',i,'80f')
  lags=rd('noise_lags',i,'24f')[(frame*8+sf*2):(frame*8+sf*2+2)]
  records.append({'packet':i//12,'frame':frame,'sf':sf,'voiced':rd('noise_voiced',i,'i')[0],
                  'sf_pulses':rd('noise_pulses',i,'h')[0],'fcbg_idx':rd('noise_fcbg',i,'h')[0],
                  'nrgres':rd('noise_nrg',i,'f')[0],'norm_br':rd('noise_nbr',i,'f')[0],
                  'seed_in':ngin['rand_seed'],'seed_out':ngout['rand_seed'],'ng_in':ngin,'ng_out':ngout,
                  'lsf':rd('noise_lsfs',i,'64f')[sf*16:(sf+1)*16],'lags':lags,'exc_pre':exc,
                  'nz':[[j,v] for j,v in enumerate(exc) if v!=0.0],'noise':rd('noise_audio',i,'80f')})
 out.write_text(json.dumps(records,separators=(',',':'))+'\n')
if __name__=='__main__':
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['spec','assemble']);p.add_argument('output',type=Path);p.add_argument('--run',type=Path);a=p.parse_args()
 if a.action=='spec':spec(a.output)
 else:assemble(a.run,a.output)
