#!/usr/bin/env python3
"""Capture HP and harmonic postfilter I/O in J#10726, without guest writes.

HP spans ops5550..5820: local33=HpPst (1332B), local19=320-sample audio.
Harmonic spans ops7728..8368: local46=HarmPst (9260B), local44=960-sample
packet, frame+4160 holds 24 lags. HarmPst is 16+17+2280 floats then two i32.
The original 220 packet/PCM hashes are retained as instrumentation guards.
"""
import argparse,json,struct
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
SPANS=[('hp_state_in',5550,33,0,1332,330),('hp_pre',5550,19,0,1280,330),
       ('hp_state_out',5820,33,0,1332,330),('hp_post',5820,19,0,1280,330),
       ('harm_state_in',7728,46,0,9260,110),('harm_pre',7728,44,0,3840,110),
       ('harm_lags',7728,11,4160,96,110),('harm_state_out',8368,46,0,9260,110),('harm_post',8368,44,0,3840,110)]

def spec(out):
 s=json.loads((ROOT/'specs/mlow_110frames.json').read_text());s['functions']['core_decode']={'index_hint':10726};s['comment']=[__doc__]
 captures=[{'op':'capture_memory','func':'core_decode','instruction':inst,'local':local,'at':at,'len':length,'count':count,'out':name} for name,inst,local,at,length,count in SPANS]
 for name,inst,local,fp,count in [('hp_lag',5820,102,True,330),('hp_len',5550,29,False,330),('harm_len',7728,41,False,110),('harm_nbr_sum',7728,112,True,110),('harm_nframes',7728,59,False,110)]:
  captures.append({'op':'capture_value','func':'core_decode','instruction':inst,'local':local,'float':fp,'count':count,'out':name})
 s['steps']=captures+s['steps'];out.write_text(json.dumps(s,indent=2)+'\n')

def assemble(run,out,harmout):
 def rd(name,i,fmt):return list(struct.unpack('<'+fmt,(run/f'{name}_{i:04}.bin').read_bytes()))
 records=[]
 for i in range(330):
  assert rd('hp_len',i,'i')[0]==320
  records.append({'frame':i,'state_in':rd('hp_state_in',i,'333f'),'state_out':rd('hp_state_out',i,'333f'),
                  'input':rd('hp_pre',i,'320f'),'output':rd('hp_post',i,'320f'),'lag':rd('hp_lag',i,'f')[0]})
 out.write_text(json.dumps(records,separators=(',',':'))+'\n');records=[]
 for i in range(110):
  assert rd('harm_len',i,'i')[0]==960
  nbr=rd('harm_nbr_sum',i,'f')[0]/rd('harm_nframes',i,'i')[0]
  records.append({'packet':i,'state_in':rd('harm_state_in',i,'2313f2i'),'state_out':rd('harm_state_out',i,'2313f2i'),
                  'input':rd('harm_pre',i,'960f'),'output':rd('harm_post',i,'960f'),'lags':rd('harm_lags',i,'24f'),'norm_br':nbr})
 harmout.write_text(json.dumps(records,separators=(',',':'))+'\n')
if __name__=='__main__':
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['spec','assemble']);p.add_argument('output',type=Path);p.add_argument('--run',type=Path);p.add_argument('--harm-output',type=Path);a=p.parse_args()
 if a.action=='spec':spec(a.output)
 else:assemble(a.run,a.output,a.harm_output)
