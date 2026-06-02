#!/usr/bin/env python3
"""
筛选 ltp-musl.csv 中 Total-score 为 0 的测例，
输出为 Rust 数组格式，方便粘贴到 SKIP_CASES 中。
"""

import csv
import sys
from pathlib import Path

CSV_PATH = Path(__file__).parent / "ltp-musl.csv"
OUT_ZERO = Path(__file__).parent / "zero_score_cases.txt"
OUT_NONZERO = Path(__file__).parent / "nonzero_score_cases.txt"


def filter_by_score(csv_path: Path, zero: bool = True) -> list[str]:
    """筛选测例。zero=True 返回总分为 0 的，zero=False 返回总分>0 的。"""
    result = []
    with open(csv_path, newline="") as f:
        reader = csv.DictReader(f)
        for row in reader:
            name = row["name"].strip()
            total_score = row.get("Total-score", "").strip()
            if total_score == "-":
                continue  # 跳过无数据行
            if (total_score == "0") == zero:
                result.append(name)
    return result


def fmt_rust(cases: list[str], const_name: str = "SKIP_CASES") -> str:
    """格式化为 Rust 的数组形式。"""
    lines = [f'    const {const_name}: &[&str] = &[']
    for case in cases:
        lines.append(f'        "{case}",')
    lines.append("    ];")
    return "\n".join(lines)


def fmt_plain(cases: list[str]) -> str:
    """每行一个测例名。"""
    return "\n".join(cases)


def main():
    # 筛选零分和有分测例
    zero_cases = filter_by_score(CSV_PATH, zero=True)
    nonzero_cases = filter_by_score(CSV_PATH, zero=False)

    # 选择输出格式
    if "--plain" in sys.argv:
        zero_out = fmt_plain(zero_cases)
        nonzero_out = fmt_plain(nonzero_cases)
    else:
        zero_out = fmt_rust(zero_cases, "SKIP_CASES")
        nonzero_out = fmt_rust(nonzero_cases, "RUN_CASES")

    # 写入文件
    OUT_ZERO.write_text(zero_out, encoding="utf-8")
    OUT_NONZERO.write_text(nonzero_out, encoding="utf-8")
    print(f"零分测例: {len(zero_cases)} 个 -> {OUT_ZERO}")
    print(f"有分测例: {len(nonzero_cases)} 个 -> {OUT_NONZERO}")


if __name__ == "__main__":
    main()
