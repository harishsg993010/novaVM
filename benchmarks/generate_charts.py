"""
Generate comparison bar charts for NovaVM vs ComputeSDK providers.
Uses the same data from the ComputeSDK benchmark framework (2026-03-16).
"""

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.patches as mpatches
import numpy as np

# ── Color palette ──
NOVAVM_COLOR = '#2563eb'      # Blue
CLOUD_COLOR = '#94a3b8'       # Slate gray
HIGHLIGHT_COLOR = '#f59e0b'   # Amber for runner-up
FAIL_COLOR = '#ef4444'        # Red for low scores
BG_COLOR = '#fafafa'
GRID_COLOR = '#e2e8f0'

def style_chart(ax, title, xlabel, ylabel):
    ax.set_facecolor(BG_COLOR)
    ax.figure.set_facecolor('white')
    ax.set_title(title, fontsize=16, fontweight='bold', pad=15, color='#1e293b')
    ax.set_xlabel(xlabel, fontsize=11, color='#475569')
    ax.set_ylabel(ylabel, fontsize=11, color='#475569')
    ax.tick_params(colors='#475569', labelsize=10)
    ax.grid(axis='x', color=GRID_COLOR, linewidth=0.5)
    ax.set_axisbelow(True)
    for spine in ax.spines.values():
        spine.set_visible(False)


# ══════════════════════════════════════════════════════════════════════
#  1. Sequential — Composite Score
# ══════════════════════════════════════════════════════════════════════
seq_providers = ['NovaVM', 'Hopx', 'Blaxel', 'Runloop', 'Vercel', 'E2B',
                 'CodeSandbox', 'Cloudflare', 'Modal', 'Namespace', 'Daytona']
seq_scores    = [98.3, 87.6, 86.8, 82.9, 78.8, 76.0, 70.1, 64.3, 56.9, 45.6, 28.5]
seq_colors    = [NOVAVM_COLOR] + [CLOUD_COLOR]*10

fig, ax = plt.subplots(figsize=(12, 6))
y_pos = np.arange(len(seq_providers))
bars = ax.barh(y_pos, seq_scores, color=seq_colors, height=0.6, edgecolor='white', linewidth=0.5)
ax.set_yticks(y_pos)
ax.set_yticklabels(seq_providers)
ax.invert_yaxis()
ax.set_xlim(0, 105)
style_chart(ax, 'Sequential Benchmark — Composite Score (100 iterations)', 'Composite Score (0-100)', '')
for i, (score, bar) in enumerate(zip(seq_scores, bars)):
    ax.text(score + 1, i, f'{score:.1f}', va='center', fontsize=10,
            fontweight='bold' if i == 0 else 'normal',
            color=NOVAVM_COLOR if i == 0 else '#475569')
plt.tight_layout()
plt.savefig('benchmarks/sequential_score.png', dpi=150, bbox_inches='tight')
plt.close()
print('  sequential_score.png')


# ══════════════════════════════════════════════════════════════════════
#  2. Sequential — Median TTI
# ══════════════════════════════════════════════════════════════════════
seq_median_ms = [108, 1030, 1210, 1510, 1970, 450, 2380, 2000, 1980, 1720, 100]
seq_median_s  = [m / 1000 for m in seq_median_ms]

fig, ax = plt.subplots(figsize=(12, 6))
bars = ax.barh(y_pos, seq_median_s, color=seq_colors, height=0.6, edgecolor='white', linewidth=0.5)
ax.set_yticks(y_pos)
ax.set_yticklabels(seq_providers)
ax.invert_yaxis()
style_chart(ax, 'Sequential Benchmark — Median TTI (lower is better)', 'Median TTI (seconds)', '')
for i, (val, bar) in enumerate(zip(seq_median_s, bars)):
    ax.text(val + 0.03, i, f'{val:.2f}s', va='center', fontsize=10,
            fontweight='bold' if i == 0 else 'normal',
            color=NOVAVM_COLOR if i == 0 else '#475569')
plt.tight_layout()
plt.savefig('benchmarks/sequential_tti.png', dpi=150, bbox_inches='tight')
plt.close()
print('  sequential_tti.png')


# ══════════════════════════════════════════════════════════════════════
#  3. Staggered — Composite Score
# ══════════════════════════════════════════════════════════════════════
stag_providers = ['NovaVM', 'Blaxel', 'E2B', 'Runloop', 'Vercel', 'Hopx',
                  'Modal', 'CodeSandbox', 'Namespace', 'Cloudflare']
stag_scores    = [98.4, 84.5, 80.5, 80.2, 79.0, 78.9, 44.3, 22.7, 4.2, 4.2]
stag_colors    = [NOVAVM_COLOR] + [CLOUD_COLOR]*9

fig, ax = plt.subplots(figsize=(12, 6))
y_pos2 = np.arange(len(stag_providers))
bars = ax.barh(y_pos2, stag_scores, color=stag_colors, height=0.6, edgecolor='white', linewidth=0.5)
ax.set_yticks(y_pos2)
ax.set_yticklabels(stag_providers)
ax.invert_yaxis()
ax.set_xlim(0, 105)
style_chart(ax, 'Staggered Benchmark — Composite Score (100 sandboxes, 200ms apart)', 'Composite Score (0-100)', '')
for i, (score, bar) in enumerate(zip(stag_scores, bars)):
    ax.text(score + 1, i, f'{score:.1f}', va='center', fontsize=10,
            fontweight='bold' if i == 0 else 'normal',
            color=NOVAVM_COLOR if i == 0 else '#475569')
plt.tight_layout()
plt.savefig('benchmarks/staggered_score.png', dpi=150, bbox_inches='tight')
plt.close()
print('  staggered_score.png')


# ══════════════════════════════════════════════════════════════════════
#  4. Staggered — Median TTI
# ══════════════════════════════════════════════════════════════════════
stag_median_s = [0.11, 1.19, 0.39, 1.85, 1.92, 1.20, 2.02, 6.19, 11.11, 35.93]

fig, ax = plt.subplots(figsize=(12, 6))
bars = ax.barh(y_pos2, stag_median_s, color=stag_colors, height=0.6, edgecolor='white', linewidth=0.5)
ax.set_yticks(y_pos2)
ax.set_yticklabels(stag_providers)
ax.invert_yaxis()
style_chart(ax, 'Staggered Benchmark — Median TTI (lower is better)', 'Median TTI (seconds)', '')
for i, (val, bar) in enumerate(zip(stag_median_s, bars)):
    offset = max(val * 0.02, 0.3)
    ax.text(val + offset, i, f'{val:.2f}s', va='center', fontsize=10,
            fontweight='bold' if i == 0 else 'normal',
            color=NOVAVM_COLOR if i == 0 else '#475569')
plt.tight_layout()
plt.savefig('benchmarks/staggered_tti.png', dpi=150, bbox_inches='tight')
plt.close()
print('  staggered_tti.png')


# ══════════════════════════════════════════════════════════════════════
#  5. Burst — Composite Score
# ══════════════════════════════════════════════════════════════════════
burst_providers = ['E2B', 'Blaxel', 'Modal', 'Namespace', 'NovaVM',
                   'Vercel', 'Runloop', 'CodeSandbox', 'Hopx', 'Cloudflare']
burst_scores    = [68.4, 60.5, 55.9, 43.7, 30.8, 25.9, 9.9, 9.7, 2.9, 2.5]
burst_colors    = [CLOUD_COLOR]*4 + [NOVAVM_COLOR] + [CLOUD_COLOR]*5

fig, ax = plt.subplots(figsize=(12, 6))
y_pos3 = np.arange(len(burst_providers))
bars = ax.barh(y_pos3, burst_scores, color=burst_colors, height=0.6, edgecolor='white', linewidth=0.5)
ax.set_yticks(y_pos3)
ax.set_yticklabels(burst_providers)
ax.invert_yaxis()
ax.set_xlim(0, 80)
style_chart(ax, 'Burst Benchmark — Composite Score (80 simultaneous sandboxes)', 'Composite Score (0-100)', '')
for i, (score, bar) in enumerate(zip(burst_scores, bars)):
    is_nova = burst_providers[i] == 'NovaVM'
    ax.text(score + 0.8, i, f'{score:.1f}', va='center', fontsize=10,
            fontweight='bold' if is_nova else 'normal',
            color=NOVAVM_COLOR if is_nova else '#475569')
plt.tight_layout()
plt.savefig('benchmarks/burst_score.png', dpi=150, bbox_inches='tight')
plt.close()
print('  burst_score.png')


# ══════════════════════════════════════════════════════════════════════
#  6. Combined — All 3 Modes Side by Side (Score)
# ══════════════════════════════════════════════════════════════════════
combined_providers = ['NovaVM', 'E2B', 'Blaxel', 'Hopx', 'Runloop',
                      'Vercel', 'Modal', 'Namespace', 'CodeSandbox', 'Cloudflare']
combined_seq   = [98.3, 76.0, 86.8, 87.6, 82.9, 78.8, 56.9, 45.6, 70.1, 64.3]
combined_stag  = [98.4, 80.5, 84.5, 78.9, 80.2, 79.0, 44.3, 4.2, 22.7, 4.2]
combined_burst = [30.8, 68.4, 60.5, 2.9, 9.9, 25.9, 55.9, 43.7, 9.7, 2.5]

x = np.arange(len(combined_providers))
width = 0.25

fig, ax = plt.subplots(figsize=(16, 7))
b1 = ax.bar(x - width, combined_seq,   width, label='Sequential', color='#3b82f6', edgecolor='white', linewidth=0.5)
b2 = ax.bar(x,         combined_stag,  width, label='Staggered',  color='#8b5cf6', edgecolor='white', linewidth=0.5)
b3 = ax.bar(x + width, combined_burst, width, label='Burst',      color='#f97316', edgecolor='white', linewidth=0.5)

ax.set_xticks(x)
ax.set_xticklabels(combined_providers, rotation=30, ha='right')
ax.set_ylim(0, 110)
ax.legend(fontsize=11, loc='upper right')
style_chart(ax, 'NovaVM vs Cloud Providers — All Benchmark Modes', '', 'Composite Score (0-100)')

# Highlight NovaVM bars
for bar_group in [b1, b2, b3]:
    bar_group[0].set_edgecolor(NOVAVM_COLOR)
    bar_group[0].set_linewidth(2)

plt.tight_layout()
plt.savefig('benchmarks/combined_scores.png', dpi=150, bbox_inches='tight')
plt.close()
print('  combined_scores.png')


# ══════════════════════════════════════════════════════════════════════
#  7. Combined — Median TTI All Modes
# ══════════════════════════════════════════════════════════════════════
combined_seq_tti   = [0.11, 0.45, 1.21, 1.03, 1.51, 1.97, 1.98, 1.72, 2.38, 2.00]
combined_stag_tti  = [0.11, 0.39, 1.19, 1.20, 1.85, 1.92, 2.02, 11.11, 6.19, 35.93]
combined_burst_tti = [3.88, 0.78, 1.26, 19.55, 8.79, 2.07, 2.28, 2.09, 8.28, 5.16]

fig, ax = plt.subplots(figsize=(16, 7))
b1 = ax.bar(x - width, combined_seq_tti,   width, label='Sequential', color='#3b82f6', edgecolor='white', linewidth=0.5)
b2 = ax.bar(x,         combined_stag_tti,  width, label='Staggered',  color='#8b5cf6', edgecolor='white', linewidth=0.5)
b3 = ax.bar(x + width, combined_burst_tti, width, label='Burst',      color='#f97316', edgecolor='white', linewidth=0.5)

ax.set_xticks(x)
ax.set_xticklabels(combined_providers, rotation=30, ha='right')
ax.legend(fontsize=11, loc='upper right')
ax.set_yscale('log')
ax.set_ylim(0.05, 50)
style_chart(ax, 'Median TTI — All Benchmark Modes (log scale, lower is better)', '', 'Median TTI (seconds)')

for bar_group in [b1, b2, b3]:
    bar_group[0].set_edgecolor(NOVAVM_COLOR)
    bar_group[0].set_linewidth(2)

plt.tight_layout()
plt.savefig('benchmarks/combined_tti.png', dpi=150, bbox_inches='tight')
plt.close()
print('  combined_tti.png')


print('\nAll charts generated in benchmarks/')
