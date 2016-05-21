# Scopes

## Representation ideas:
- Normal arrays of strings
- array of 32-bit or 64-bit atoms (maybe using Servo's atom library)
- Atoms packed into one or two u64s
  - fast equality checking
  - potentially fast prefix checking
  - needs unsafe code

## Potential packings:
- variable width atoms, either 7 bits and a tag bit for top 128 or 13 bits and 3 tagging bits for rest
  - can fit all but 33 of the scopes present
- tagged pointer (taking advantage of alignment), either a pointer to a slow path, or the first 4 bits set then a packed representation, one of others mentioned
- 6 10-bit atoms referencing unique things by position (see by-position stats below)
- 5 11-bit atoms and one 8-bit one for the first atom (2^11 = 2048, 2^8 = 256), one remaining bit for tag marker

## Stats:
- 7000 scopes referenced in sublime, 3537 unique ones, all stats after this are based on non-unique data
- all but 33 scopes in default packages could fit in 64 with combination 8bit or 16bit atom encoding
- there are only 1219 unique atoms in the default package set
- the top 128 atoms make up ~90% of all unique atoms referenced in syntax files
- there are 26 unique first atoms, 145 unique last atoms
- every position (1st atom, 2nd atom, ...) has under 878 possibilities, only 2nd,3rd and 4th have >256
- 99.8% of scopes have 6 or fewer atoms, 97% have 5 or fewer, 70% have 4 or fewer
  - for unique scopes: {2=>81, 4=>1752, 3=>621, 5=>935, 7=>8, 6=>140} ----> 95% of uniques <= 6
  - for non-unique scopes: {2=>125, 4=>3383, 3=>1505, 5=>1891, 7=>9, 6=>202}
