#!/usr/bin/env python3
"""実物の iap.yaml から plan のパリティ用ケースを作る（読み取り専用の plan で
create / drift / listing の分岐を通すため）。外部ライブラリを使わず行単位で書き換える。

usage: make_cases.py <iap.yaml> <出力ディレクトリ>
"""
import re
import sys
from pathlib import Path

src, out = Path(sys.argv[1]), Path(sys.argv[2])
out.mkdir(parents=True, exist_ok=True)
base = src.read_text()


def bump_prices(s: str) -> str:
    # 全商品の price を +1（サブスクの base plan 価格と買い切りの両方で差分を出す）
    return re.sub(r"(^\s+price:\s*)([0-9.]+)", lambda m: f"{m.group(1)}{float(m.group(2)) + 1:g}", s, flags=re.M)


def change_first_description(s: str) -> str:
    return re.sub(r'(^\s+description:\s*")([^"]*)(")', r"\1\2 (parity)\3", s, count=1, flags=re.M)


PROBE = """  - key: parityprobe
    appStoreId: parity.probe.notexist
    playProductId: parity_probe_notexist
    playBasePlanId: parityprobe
    period: P1M
    price: 1.99
    localizations:
      - locale: en-US
        name: "Parity probe"
        description: "parity probe"
"""


def add_probe(s: str) -> str:
    return re.sub(r"^products:\s*\n", "products:\n" + PROBE, s, count=1, flags=re.M)


cases = {
    "00-baseline": base,
    "01-price-drift": bump_prices(base),
    "02-new-product": add_probe(base),
    "03-listing-change": change_first_description(base),
}
for name, body in cases.items():
    if name != "00-baseline" and body == base:
        sys.exit(f"{name}: 書き換える箇所が見つからない（iap.yaml の形を確認）")
    (out / f"{name}.yaml").write_text(body)
    print(out / f"{name}.yaml")
