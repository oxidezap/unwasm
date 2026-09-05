#!/usr/bin/env python3
"""Trace window/LPC/BWE directly from all 330 frames of the shipped encoder.

J#10736: window call op1375, LPC call op1394, BWE loop ends before op1422.
The pre-BWE and post-BWE A are both captured, so no host implements the
oracle's bandwidth-expansion step. Baseline output hashes remain mandatory.
"""
import argparse,json,struct
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
SPANS=[('fe_input',1375,18,0,1792),('fe_window',1394,11,1472,1792),
       ('fe_a_raw',1395,9,0,68),('fe_a',1422,9,0,68),('fe_r',1395,11,1328,136),('fe_f2',1395,11,288,1028)]

def spec(out):
 s=json.loads((ROOT/'specs/mlow_110frames.json').read_text());s['functions']['core_encode']={'index_hint':10736};s['comment']=[__doc__]
 captures=[{'op':'capture_memory','func':'core_encode','instruction':inst,'local':local,'at':at,'len':length,'count':330,'out':name} for name,inst,local,at,length in SPANS]
 captures.append({'op':'capture_value','func':'core_encode','instruction':1375,'local':17,'count':330,'out':'fe_frame'})
 s['steps']=captures+s['steps'];out.write_text(json.dumps(s,indent=2)+'\n')
def assemble(run,out):
 def rd(name,i,fmt):return list(struct.unpack('<'+fmt,(run/f'{name}_{i:04}.bin').read_bytes()))
 records=[]
 for i in range(330):
  frame=rd('fe_frame',i,'i')[0];assert frame==i%3
  records.append({'pkt':i//3,'numframe':frame,'lpcbuf':rd('fe_input',i,'448f'),'windowed':rd('fe_window',i,'448f'),
                  'A_before_bwe':rd('fe_a_raw',i,'17f'),'A':rd('fe_a',i,'17f'),'R':rd('fe_r',i,'17d'),'F2':rd('fe_f2',i,'257f')})
 out.write_text(json.dumps(records,separators=(',',':'))+'\n')
if __name__=='__main__':
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['spec','assemble']);p.add_argument('output',type=Path);p.add_argument('--run',type=Path);a=p.parse_args()
 if a.action=='spec':spec(a.output)
 else:assemble(a.run,a.output)
