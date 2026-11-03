# SPDX-License-Identifier: GPL-3.0-or-later
import os, pathlib, signal, subprocess, sys
root=pathlib.Path.cwd()
(root/'target/ci/macos').mkdir(parents=True, exist_ok=True)
binary=next(p for p in (root/'target/debug/deps').glob('macos_native-*') if p.is_file() and p.suffix=='' and os.access(p,os.X_OK))
env={k:v for k,v in os.environ.items() if not k.startswith('FLOWMUX_')}
env.update(GSK_RENDERER='cairo',G_DEBUG='fatal-criticals',RUST_BACKTRACE='1',FLOWMUX_BUNDLED_CLI_PATH=str(root/'target/debug/flowmuxctl'),FLOWMUX_TERMINATION_SMOKE_ONLY='1')
for attempt in range(1,251):
 log=root/f'target/ci/macos/native-{attempt}.log'
 with log.open('w') as stream:
  p=subprocess.Popen([str(binary)],env=env,stdout=stream,stderr=subprocess.STDOUT,start_new_session=True)
  try:
   result=p.wait(timeout=35)
  except subprocess.TimeoutExpired:
   subprocess.run(['/usr/bin/sample',str(p.pid),'3','1','-file',str(log)+'.sample'],timeout=15,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
   os.killpg(p.pid,signal.SIGKILL); p.wait(); result=124
  print(f'Attempt {attempt}: exit={result} log={log}',flush=True)
  print('\n'.join(line for line in log.read_text().splitlines() if line.startswith('MACOS_') or line.startswith('OPTIONS_')),flush=True)
  if result:
   print(log.read_text()[-6000:],flush=True);sys.exit(result)
print('TERMINATION_REPEAT_250_OK',flush=True)
