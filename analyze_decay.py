import re
import sys
from collections import defaultdict

def analyze_decay(log_file):
    # Tracks decay metrics: {symbol: [decay_ms_list]}
    decay_data = defaultdict(list)
    
    # 2026-03-31T17:05:29.158Z  INFO ThreadId(01) tokioparasite: ALPHA_DECAY: DOGE | decay_ms=524.39 | target=0.0913 | side=Sell (from book)
    decay_re = re.compile(r"ALPHA_DECAY: (\S+) \| decay_ms=([\d.]+) \| target=[\d.]+", re.IGNORECASE)

    with open(log_file, 'r') as f:
        for line in f:
            match = decay_re.search(line)
            if match:
                symbol, ms = match.groups()
                decay_data[symbol].append(float(ms))
    
    if not decay_data:
        print("No ALPHA_DECAY logs found.")
        return

    print("\n--- ALPHA DECAY BY SYMBOL (10-MIN RUN) ---")
    print(f"{'SYMBOL':<8} | {'SAMPLES':<8} | {'AVG (ms)':<10} | {'MIN (ms)':<10} | {'MAX (ms)':<10}")
    print("-" * 60)
    
    all_ms = []
    for s in sorted(decay_data.keys()):
        ms_list = decay_data[s]
        all_ms.extend(ms_list)
        avg = sum(ms_list) / len(ms_list)
        mink = min(ms_list)
        maxk = max(ms_list)
        print(f"{s:<8} | {len(ms_list):<8} | {avg:<10.2f} | {mink:<10.2f} | {maxk:<10.2f}")
    
    if all_ms:
        global_avg = sum(all_ms) / len(all_ms)
        print("-" * 60)
        print(f"{'GLOBAL':<8} | {len(all_ms):<8} | {global_avg:<10.2f} | {min(all_ms):<10.2f} | {max(all_ms):<10.2f}")

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python analyze_decay.py <log_file>")
    else:
        analyze_decay(sys.argv[1])
