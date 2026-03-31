import re
import sys
from collections import defaultdict

def analyze_v2(log_file):
    entries = {}
    trades = []
    
    entry_re = re.compile(r"ORDER_ENTRY: (BUY|SELL) (\S+) \| price=([\d.]+) \| r=([\d.]+) \| id=ORD-(\d+)")
    exit_re = re.compile(r"ORDER_EXIT: (BUY|SELL) (\S+) \| price=([\d.]+) \| id=ORD-(\d+)")
    
    # Track cumulative PnL for chart data (sampled every 10 trades)
    cum_pnl = 0.0
    pnl_history = [0.0]
    
    with open(log_file, 'r') as f:
        for line in f:
            e_match = entry_re.search(line)
            if e_match:
                side, symbol, price, r, oid = e_match.groups()
                entries[symbol] = {'side': side, 'entry_price': float(price), 'r': float(r)}
                continue
                
            x_match = exit_re.search(line)
            if x_match:
                side, symbol, price, oid = x_match.groups()
                if symbol in entries:
                    ent = entries.pop(symbol)
                    entry_p = ent['entry_price']
                    exit_p = float(price)
                    
                    if ent['side'] == 'BUY':
                        raw_bps = (exit_p - entry_p) / entry_p * 10000.0
                    else:
                        raw_bps = (entry_p - exit_p) / entry_p * 10000.0
                        
                    # Estimation: 2.5bps half-spread * 2 + 2.5bps fee * 2 = 10bps friction
                    # The simulator matches against the side, so spread is already in prices.
                    # But ORD_ENTRY log price is the execution price (with spread).
                    # So raw_bps is the real captured price move.
                    # We only subtract fees (5.0 bps total).
                    net_bps = raw_bps - 5.0 
                    
                    cum_pnl += net_bps
                    pnl_history.append(cum_pnl)
                    
                    trades.append({
                        'symbol': symbol,
                        'alpha': ent['r'] * 18.0,
                        'realized': raw_bps,
                        'net': net_bps
                    })

    if not trades: return
    
    # Calculate per-symbol stats
    stats = {}
    for t in trades:
        s = t['symbol']
        if s not in stats: stats[s] = {'alpha': [], 'realized': [], 'net': [], 'wins': 0}
        stats[s]['alpha'].append(t['alpha'])
        stats[s]['realized'].append(t['realized'])
        stats[s]['net'].append(t['net'])
        if t['net'] > 0: stats[s]['wins'] += 1

    print("SUMMARY_TABLE_START")
    for s in sorted(stats.keys()):
        st = stats[s]
        n = len(st['net'])
        avg_a = sum(st['alpha'])/n
        avg_r = sum(st['realized'])/n
        avg_n = sum(st['net'])/n
        wr = st['wins']/n * 100
        print(f"{s},{n},{avg_a:.2f},{avg_r:.2f},{avg_n:.2f},{wr:.1f}")
    
    print("CHART_DATA_START")
    # Downsample PnL history to 20 points for Mermaid
    step = max(1, len(pnl_history)//20)
    sampled = pnl_history[::step]
    print(",".join(map(lambda x: f"{x:.1f}", sampled)))

if __name__ == "__main__":
    analyze_v2(sys.argv[1])
