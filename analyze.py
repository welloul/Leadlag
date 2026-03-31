import re
import sys
from collections import defaultdict

def analyze_logs(log_file):
    entries = {} # (symbol, venue) -> entry_data
    trades = []
    
    # Regex to match the logs I added in main.rs
    entry_re = re.compile(r"ORDER_ENTRY: (BUY|SELL) (\S+) \| price=([\d.]+) \| r=([\d.]+) \| id=ORD-(\d+)")
    exit_re = re.compile(r"ORDER_EXIT: (BUY|SELL) (\S+) \| price=([\d.]+) \| id=ORD-(\d+)")
    
    with open(log_file, 'r') as f:
        for line in f:
            e_match = entry_re.search(line)
            if e_match:
                side, symbol, price, r, oid = e_match.groups()
                entries[symbol] = {
                    'side': side,
                    'entry_price': float(price),
                    'r': float(r),
                    'entry_id': oid
                }
                continue
                
            x_match = exit_re.search(line)
            if x_match:
                side, symbol, price, oid = x_match.groups()
                if symbol in entries:
                    ent = entries.pop(symbol)
                    entry_p = ent['entry_price']
                    exit_p = float(price)
                    
                    # Calculate Realized BPS
                    # If BUY entry, we want price to go UP (Exit > Entry)
                    if ent['side'] == 'BUY':
                        raw_bps = (exit_p - entry_p) / entry_p * 10000.0
                    else:
                        raw_bps = (entry_p - exit_p) / entry_p * 10000.0
                        
                    alpha_bps = ent['r'] * 7.0 # Threshold is 7.0
                    net_bps = raw_bps - 5.0 # Total fee estimation (2.5 x 2)
                    
                    trades.append({
                        'symbol': symbol,
                        'alpha': alpha_bps,
                        'realized': raw_bps,
                        'net': net_bps,
                        'entry_p': entry_p,
                        'exit_p': exit_p
                    })

    if not trades:
        print("No paired trades found in logs.")
        return

    # Aggregate by symbol
    stats = defaultdict(list)
    for t in trades:
        stats[t['symbol']].append(t)
        
    print("| Symbol | Trades | Avg Alpha (bps) | Avg Realized (bps) | Net PnL (bps) | Success Rate |")
    print("|--------|--------|-----------------|-------------------|---------------|--------------|")
    
    total_net = 0
    total_trades = 0
    
    for sym in sorted(stats.keys()):
        sym_trades = stats[sym]
        avg_alpha = sum(t['alpha'] for t in sym_trades) / len(sym_trades)
        avg_realized = sum(t['realized'] for t in sym_trades) / len(sym_trades)
        avg_net = sum(t['net'] for t in sym_trades) / len(sym_trades)
        success = len([t for t in sym_trades if t['net'] > 0]) / len(sym_trades) * 100
        
        print(f"| {sym} | {len(sym_trades)} | {avg_alpha:.2f} | {avg_realized:.2f} | {avg_net:.2f} | {success:.1f}% |")
        total_net += sum(t['net'] for t in sym_trades)
        total_trades += len(sym_trades)
        
    print(f"\n**TOTAL TRADES:** {total_trades}")
    print(f"**AVG NET BPS PER TRADE:** {total_net/total_trades:.2f}")

if __name__ == "__main__":
    if len(sys.argv) > 1:
        analyze_logs(sys.argv[1])
    else:
        print("Usage: python3 analyze.py <log_file>")
