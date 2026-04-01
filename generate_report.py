import sys
import re
from collections import defaultdict

if len(sys.argv) < 2:
    print("Usage: python3 generate_report.py <logfile>")
    sys.exit(1)

log_file = sys.argv[1]

# Data structures
stats = defaultdict(lambda: {
    'decays': [],
    'time_exits': 0,
})

latest_positions = {}
total_fills = 0

with open(log_file, 'r') as f:
    for line in f:
        # Match ALPHA_DECAY:
        if "ALPHA_DECAY:" in line:
            m = re.search(r'ALPHA_DECAY:\s+([A-Z]+)(?:USDT)?\s+\|\s+decay_ms=([\d\.]+)', line)
            if m:
                sym = m.group(1).replace('USDT', '')
                decay = float(m.group(2))
                stats[sym]['decays'].append(decay)
                
        # Match TIME EXIT
        elif "TIME EXIT" in line:
            m = re.search(r'TIME EXIT:\s+([A-Z]+)', line)
            if m:
                sym = m.group(1).replace('USDT', '')
                stats[sym]['time_exits'] += 1
                
        # Match POSITIONS block
        elif "POSITIONS (" in line:
            m = re.search(r'POSITIONS \((\d+) fills', line)
            if m:
                total_fills = int(m.group(1))
                
        # Match Venue PnL
        elif "VenueId(1) " in line and "uPnL=" in line:
            m = re.search(r'VenueId\(1\)\s+([A-Z]+)\s+\|-*.*?uPnL=([\-\d\.]+)', line)
            if m:
                sym = m.group(1)
                pnl = float(m.group(2))
                latest_positions[sym] = pnl

# Output table
print(f"{'Symbol':<6} | {'Min AD(ms)':<10} | {'Avg AD(ms)':<10} | {'Max AD(ms)':<10} | {'PnL ($)':<8} | {'Time Exits':<10} | {'Decay / TP Hits':<15}")
print("-" * 85)

symbols = sorted(list(set(stats.keys()) | set(latest_positions.keys())))
for sym in symbols:
    d = stats[sym]['decays']
    if d:
        min_d = f"{min(d):.1f}"
        avg_d = f"{sum(d)/len(d):.1f}"
        max_d = f"{max(d):.1f}"
        hits = len(d)
    else:
        min_d = avg_d = max_d = "-"
        hits = 0
        
    pnl = f"{latest_positions.get(sym, 0.0):.2f}"
    tx = stats[sym]['time_exits']
    
    print(f"{sym:<6} | {min_d:<10} | {avg_d:<10} | {max_d:<10} | {pnl:<8} | {tx:<10} | {hits:<15}")

print("-" * 85)
print(f"Total Combined Fills (Entries + TPs + Time Exits): {total_fills}")
