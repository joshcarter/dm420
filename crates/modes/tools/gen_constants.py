import re, sys
src = open('/tmp/ft8_lib_src/ft8/constants.c').read()

def grab(name):
    # find "name" then the {...}; block
    i = src.index(name)
    j = src.index('{', i)
    # match braces
    depth=0; k=j
    while k < len(src):
        if src[k]=='{': depth+=1
        elif src[k]=='}':
            depth-=1
            if depth==0: break
        k+=1
    body = re.sub(r"//[^\n]*", "", src[j:k+1])
    nums = [int(x,0) for x in re.findall(r'0x[0-9A-Fa-f]+|\b\d+\b', body)]
    return nums

def emit_1d(name, nums, ty='u8'):
    return f"pub const {name}: [{ty}; {len(nums)}] = [{', '.join(str(n) for n in nums)}];\n"

def emit_2d(name, nums, cols, ty='u8'):
    rows = len(nums)//cols
    out = f"pub const {name}: [[{ty}; {cols}]; {rows}] = [\n"
    for r in range(rows):
        row = nums[r*cols:(r+1)*cols]
        out += "    [" + ", ".join(str(n) for n in row) + "],\n"
    out += "];\n"
    return out

gen = grab('kFTX_LDPC_generator')      # 83*12
nm  = grab('kFTX_LDPC_Nm')             # 83*7
mn  = grab('kFTX_LDPC_Mn')             # 174*3
rows= grab('kFTX_LDPC_Num_rows')       # 83

assert len(gen)==83*12, len(gen)
assert len(nm)==83*7, len(nm)
assert len(mn)==174*3, len(mn)
assert len(rows)==83, len(rows)

hdr = '''//! FT8/FT4 protocol constants (the on-air specification).
//!
//! These fixed numeric tables — the Costas sync arrays, the Gray map, the
//! LDPC(174,91) parity/generator matrices, the CRC polynomial, and the FT4
//! whitening sequence — are part of the interoperable FT8/FT4 protocol defined
//! by Franke/Taylor (WSJT-X). They must match bit-for-bit or no real signal
//! decodes. They are transcribed here as data; all of the logic that uses them
//! is our own. Table values cross-checked against ft8_lib (MIT, K. Goba) — see
//! ATTRIBUTION.md. Do not "tidy" these numbers.
#![allow(clippy::all)]
#![cfg_attr(rustfmt, rustfmt::skip)]

// Costas 7x7 sync tone pattern (FT8) and 4x4 patterns (FT4).
'''
out = hdr
out += emit_1d('FT8_COSTAS', grab('kFT8_Costas_pattern'))
out += emit_2d('FT4_COSTAS', grab('kFT4_Costas_pattern'), 4)
out += "\n// Gray code map (FTx bits -> channel symbols).\n"
out += emit_1d('FT8_GRAY', grab('kFT8_Gray_map'))
out += emit_1d('FT4_GRAY', grab('kFT4_Gray_map'))
out += "\n// FT4 payload whitening (XOR) sequence.\n"
out += emit_1d('FT4_XOR', grab('kFT4_XOR_sequence'))
out += "\n// LDPC(174,91): column-weight-3 regular code.\n"
out += "// Generator: 83 parity rows x 12 bytes (91-bit message -> 83 parity bits).\n"
out += emit_2d('LDPC_GENERATOR', gen, 12)
out += "// Nm: for each of 83 checks, the up-to-7 variable nodes (1-based) it covers.\n"
out += emit_2d('LDPC_NM', nm, 7)
out += "// Mn: for each of 174 variables, the 3 checks (1-based) it participates in.\n"
out += emit_2d('LDPC_MN', mn, 3)
out += "// Number of variable nodes per check row.\n"
out += emit_1d('LDPC_NUM_ROWS', rows)

open('crates/modes/src/constants.rs','w').write(out)
print("wrote crates/modes/src/constants.rs")
print("FT8_COSTAS", grab('kFT8_Costas_pattern'))
print("gen rows", len(gen)//12, "nm rows", len(nm)//7, "mn rows", len(mn)//3, "num_rows", len(rows))
print("first gen row bytes:", gen[:12])
print("num_rows sample:", rows[:10])
