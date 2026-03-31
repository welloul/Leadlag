import re
import sys
from collections import defaultdict

def analyze_maker(log_file):
    entries = {} # symbol -> {side, price, r, entry_filled_price}
    trades = []
    
    # 1. Track Entry Intent
    # 2026-03-31T09:11:55.907Z  INFO ThreadId(01) tokioparasite: ORDER_ENTRY: BUY ARB | price=0.0918 | r=1.56 | id=ORD-0
    entry_intent_re = re.compile(r"ORDER_ENTRY: (BUY|SELL) (\S+) \| price=([\d.]+) \| r=([\d.]+) \| id=([\w-]+)", re.IGNORECASE)
    
    # 2. Track Limit Fills (Entries or TPs)
    # 2026-03-31T09:12:46.124Z  INFO ThreadId(01) tokioparasite::sim: LIMIT FILL: BUY ARB @ 0.091805
    limit_fill_re = re.compile(r"LIMIT FILL: (BUY|SELL) (\S+) @ ([\d.]+)", re.IGNORECASE)
    
    # 3. Track Market Exits (Time-based safety)
    # ORDER_EXIT: SELL BTC | price=60010.5 | id=ORD-123
    market_exit_re = re.compile(r"ORDER_EXIT: (BUY|SELL) (\S+) \| price=([\d.]+) \| id=([\w-]+)", re.IGNORECASE)

    cum_pnl = 0.0
    pnl_history = [0.0]
    
    with open(log_file, 'r') as f:
        for line in f:
            # 1. Track Entry Intent
            ei_match = entry_intent_re.search(line)
            if ei_match:
                side, symbol, price, r, oid = ei_match.groups()
                # We save the intent. We don't have the fill price yet.
                entries[symbol] = {
                    'side': side.upper(), 
                    'target_price': float(price), 
                    'r': float(r),
                    'fill_price': None,
                    'is_open': False
                }
                continue
            
            # 2. Track Limit Fills (Entries or TPs)
            lf_match = limit_fill_re.search(line)
            if lf_match:
                side, symbol, price = lf_match.groups()
                side = side.upper()
                price = float(price)
                
                # Check if this is an Entry fill
                if symbol in entries and entries[symbol]['side'] == side and not entries[symbol]['is_open']:
                    entries[symbol]['fill_price'] = price
                    entries[symbol]['is_open'] = True
                    # print(f"DEBUG: Entry fill for {symbol} at {price}")
                
                # Check if this is a TP fill (opposite side, and we are open)
                elif symbol in entries and entries[symbol]['is_open'] and entries[symbol]['side'] != side:
                    ent = entries.pop(symbol)
                    entry_p = ent['fill_price']
                    exit_p = price
                    
                    if ent['side'] == 'BUY':
                        raw_bps = (exit_p - entry_p) / entry_p * 10000.0
                    else:
                        raw_bps = (entry_p - exit_p) / entry_p * 10000.0
                    
                    # MAKER FEE is 0.0. No spread cost (we are makers).
                    net_bps = raw_bps - 0.0 
                    
                    cum_pnl += net_bps
                    pnl_history.append(cum_pnl)
                    trades.append({'symbol': symbol, 'alpha': ent['r'] * 14.0, 'realized': raw_bps, 'net': net_bps, 'exit_type': 'TP'})
                continue

            # 3. Track Market Exits (Time-based safety)
            me_match = market_exit_re.search(line)
            if me_match:
                side, symbol, price, oid = me_match.groups()
                side = side.upper()
                price = float(price)
                
                if symbol in entries and entries[symbol]['is_open']:
                    ent = entries.pop(symbol)
                    entry_p = ent['fill_price']
                    exit_p = price
                    
                    if ent['side'] == 'BUY':
                        raw_bps = (exit_p - entry_p) / entry_p * 10000.0
                    else:
                        raw_bps = (entry_p - exit_p) / entry_p * 10000.0
                    
                    # MARKET EXIT FEE (Taker) = 5.0 bps
                    net_bps = raw_bps - 5.0 
                    
                    cum_pnl += net_bps
                    pnl_history.append(cum_pnl)
                    trades.append({'symbol': symbol, 'alpha': ent['r'] * 14.0, 'realized': raw_bps, 'net': net_bps, 'exit_type': 'TIME'})
                continue

    if not trades:
        print("No trades found in log.")
        return
    
    # Calculate per-symbol stats
    stats = {}
    tp_count = 0
    time_count = 0
    
    for t in trades:
        s = t['symbol']
        if s not in stats: stats[s] = {'realized': [], 'net': [], 'wins': 0, 'tp': 0}
        stats[s]['realized'].append(t['realized'])
        stats[s]['net'].append(t['net'])
        if t['net'] > 0: stats[s]['wins'] += 1
        if t['exit_type'] == 'TP': 
            stats[s]['tp'] += 1
            tp_count += 1
        else:
            time_count += 1

    print("\n--- PERFORMANCE SUMMARY (MAKER-BOT) ---")
    print(f"Total Trades: {len(trades)}")
    print(f"TP Hits: {tp_count} ({tp_count*100/len(trades):.1f}%)")
    print(f"Time Exits: {time_count} ({time_count*100/len(trades):.1f}%)")
    print(f"Net PnL (BPS): {cum_pnl:.2f}\n")

    print("SYMBOL | TRADES | TP_RATE | AVG_NET_BPS | WIN_RATE")
    print("-" * 50)
    for s in sorted(stats.keys()):
        st = stats[s]
        n = len(st['net'])
        avg_n = sum(st['net'])/n
        tp_r = st['tp'] / n * 100
        wr = st['wins']/n * 100
        print(f"{s:6} | {n:6} | {tp_r:6.1f}% | {avg_n:10.2f} | {wr:7.1f}%")
    
    print("\nCHART_DATA_START")
    step = max(1, len(pnl_history)//20)
    sampled = pnl_history[::step]
    print(",".join(map(lambda x: f"{x:.1f}", sampled)))

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python analyze_maker.py <log_file>")
    else:
        analyze_maker(sys.argv[1])
