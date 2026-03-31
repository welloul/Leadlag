import re
import sys
from collections import defaultdict

def analyze_maker_v2(log_file):
    # Track positions per symbol: {symbol: {'size': 0.0, 'total_notional': 0.0, 'realized_pnl': 0.0}}
    positions = defaultdict(lambda: {'size': 0.0, 'cost_basis': 0.0, 'pnl_bps': 0.0, 'trades': 0, 'wins': 0})
    
    # regex for limit fills: LIMIT FILL: (BUY|SELL) (\S+) @ ([\d.]+)
    fill_re = re.compile(r"LIMIT FILL: (BUY|SELL) (\S+) @ ([\d.]+)", re.IGNORECASE)
    # regex for market exits: ORDER_EXIT: (BUY|SELL) (\S+) \| price=([\d.]+) \| id=[\w-]+
    exit_re = re.compile(r"ORDER_EXIT: (BUY|SELL) (\S+) \| price=([\d.]+) \| id=[\w-]+", re.IGNORECASE)
    
    # We also track cumulative PnL for the chart
    pnl_history = [0.0]
    total_cum_pnl = 0.0

    with open(log_file, 'r') as f:
        for line in f:
            match = fill_re.search(line)
            is_taker = False
            if not match:
                match = exit_re.search(line)
                is_taker = True
            
            if match:
                side, symbol, price = match.groups()
                price = float(price)
                side_mult = 1.0 if side.upper() == 'BUY' else -1.0
                
                pos = positions[symbol]
                
                # Check for closing/reducing trade
                if pos['size'] != 0 and (pos['size'] * side_mult < 0):
                    # Reduction or Flip
                    trade_size = abs(pos['size']) # Assuming we close fully or more
                    # For simplicity, treat every opposite fill as a close event for the previous entry.
                    # PnL (bps) = (Exit - Entry) / Entry * 10000
                    entry_p = pos['cost_basis']
                    exit_p = price
                    
                    if pos['size'] > 0: # We were LONG
                        raw_bps = (exit_p - entry_p) / entry_p * 10000.0
                    else: # We were SHORT
                        raw_bps = (entry_p - exit_p) / entry_p * 10000.0
                    
                    # Fee: 0.0 for Maker, 2.5 for Taker (on this side)
                    fee = 2.5 if is_taker else 0.0
                    net_bps = raw_bps - fee
                    
                    pos['pnl_bps'] += net_bps
                    pos['trades'] += 1
                    if net_bps > 0: pos['wins'] += 1
                    
                    total_cum_pnl += net_bps
                    pnl_history.append(total_cum_pnl)
                    
                    # Reset/Flip position (Simulator behavior: usually closes full position in our bot)
                    pos['size'] = 0.0
                    pos['cost_basis'] = 0.0
                else:
                    # Entry or Adding (Maker ONLY)
                    # We only add if it's not a taker exit
                    if not is_taker:
                        # Update average cost basis
                        old_s = abs(pos['size'])
                        new_s = 1.0 # Assuming lot size 1.0 per signal
                        pos['cost_basis'] = (pos['cost_basis'] * old_s + price * new_s) / (old_s + new_s)
                        pos['size'] += side_mult
    
    print("\n--- FINAL SCALPER ANALYSIS ---")
    print(f"Cumulative PnL: {total_cum_pnl:.2f} bps")
    print("\nSYMBOL | TRADES | WIN RATE | TOT_PNL_BPS | AVG_BPS")
    print("-" * 55)
    
    for s in sorted(positions.keys()):
        p = positions[s]
        if p['trades'] == 0: continue
        wr = p['wins']/p['trades'] * 100
        avg = p['pnl_bps'] / p['trades']
        print(f"{s:6} | {p['trades']:6} | {wr:7.1f}% | {p['pnl_bps']:11.2f} | {avg:7.2f}")
    
    print("\nCHART_DATA_START")
    step = max(1, len(pnl_history)//20)
    print(",".join(map(lambda x: f"{x:.1f}", pnl_history[::step])))

if __name__ == "__main__":
    if len(sys.argv) < 2:
        print("Usage: python analyze_maker_v2.py <log_file>")
    else:
        analyze_maker_v2(sys.argv[1])
