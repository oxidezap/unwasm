#!/usr/bin/env python3
"""Capture wire parameters and entropy context from J#10758.

LbQuantParams is p9, 1416B. Offsets: voiced0, fcbg4 (4*i16), acbg12,
LSF indices20 (17*i8), interpolation40, contour44, lags48 (8*i32),
energy symbols80/84, energy88 (4*f32), energyQ14=104 (4*i32),
legacy dense pulses120 (unused); sparse positions760/magnitudes1080
(160*i16 each), nPositions1400, nPulses1404, sfPulses1408 (4*i16).
The context at p1 is ec_dec; skip its pointer and retain eleven words at +4.
"""
import argparse,json,struct
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]

def spec(out):
 s=json.loads((ROOT/'specs/mlow_110frames.json').read_text());s['functions']['decode_params']={'index_hint':10758};s['comment']=[__doc__]
 captures=[]
 for name,local,at,length in [('params',9,0,1416),('range_after',1,4,44)]:captures.append({'op':'capture_memory','func':'decode_params','instruction':1466,'local':local,'at':at,'len':length,'count':330,'out':name})
 for name,local in [('params_len',2),('params_subframes',3),('params_cav',4),('params_lowrate',6),('params_frame',7),('params_sid',8)]:captures.append({'op':'capture_value','func':'decode_params','instruction':0,'local':local,'count':330,'out':name})
 s['steps']=captures+s['steps'];out.write_text(json.dumps(s,indent=2)+'\n')

def assemble(run,out):
 records=[]
 for i in range(330):
  raw=(run/f'params_{i:04}.bin').read_bytes()
  def rd(at,fmt):return list(struct.unpack_from('<'+fmt,raw,at))
  r={'voiced':rd(0,'i')[0],'fcbg':rd(4,'4h'),'acbg':rd(12,'4h'),'lsf':rd(20,'17b'),'interp':rd(40,'i')[0],
     'contour':rd(44,'i')[0],'lags':rd(48,'8i'),'energy_q14':rd(104,'4i'),'pulses':None,'sf_pulses':rd(1408,'4h')}
  pulses=[0]*320
  positions=rd(760,'160h');magnitudes=rd(1080,'160h');count=rd(1400,'i')[0]
  assert 0 <= count <= 160
  for pos,mag in zip(positions[:count],magnitudes[:count]):
   assert 0 <= pos < 320
   pulses[pos] += mag
  r['pulses']=pulses
  for name in ['len','subframes','cav','lowrate','frame','sid']:
   r[name]=struct.unpack('<i',(run/f'params_{name}_{i:04}.bin').read_bytes())[0]
  r['range']=list(struct.unpack('<11I',(run/f'range_after_{i:04}.bin').read_bytes()))
  records.append(r)
 out.write_text(json.dumps(records,separators=(',',':'))+'\n')
if __name__=='__main__':
 p=argparse.ArgumentParser(description=__doc__);p.add_argument('action',choices=['spec','assemble']);p.add_argument('output',type=Path);p.add_argument('--run',type=Path);a=p.parse_args()
 if a.action=='spec':spec(a.output)
 else:assemble(a.run,a.output)
