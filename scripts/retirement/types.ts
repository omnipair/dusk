/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/dusk.json`.
 */
export type Dusk = {
  "address": "DuskWhswJYrzABDn8GqQD191X9RobRi5jhgGntXEdnKj",
  "metadata": {
    "name": "dusk",
    "version": "2.0.0",
    "spec": "0.1.0",
    "description": "Dusk market architecture program for Omnipair V2"
  },
  "instructions": [
    {
      "name": "abortStaleLegacyFreeze",
      "discriminator": [
        215,
        45,
        137,
        66,
        15,
        71,
        73,
        50
      ],
      "accounts": [
        {
          "name": "authority",
          "writable": true,
          "signer": true,
          "address": "EdYBBhu2PU84eNt6r8zPWkaxrxbXVAzVvAz3tR3XveRq"
        },
        {
          "name": "frozen",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  103,
                  97,
                  99,
                  121,
                  95,
                  102,
                  114,
                  101,
                  101,
                  122,
                  101
                ]
              },
              {
                "kind": "const",
                "value": [
                  103,
                  104,
                  33,
                  254,
                  53,
                  66,
                  172,
                  146,
                  108,
                  179,
                  114,
                  56,
                  133,
                  170,
                  139,
                  129,
                  24,
                  11,
                  46,
                  78,
                  178,
                  50,
                  184,
                  47,
                  5,
                  123,
                  200,
                  48,
                  237,
                  243,
                  13,
                  186
                ]
              }
            ]
          }
        },
        {
          "name": "retirement",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  103,
                  97,
                  99,
                  121,
                  95,
                  114,
                  101,
                  116,
                  105,
                  114,
                  101,
                  109,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "const",
                "value": [
                  103,
                  104,
                  33,
                  254,
                  53,
                  66,
                  172,
                  146,
                  108,
                  179,
                  114,
                  56,
                  133,
                  170,
                  139,
                  129,
                  24,
                  11,
                  46,
                  78,
                  178,
                  50,
                  184,
                  47,
                  5,
                  123,
                  200,
                  48,
                  237,
                  243,
                  13,
                  186
                ]
              }
            ]
          }
        },
        {
          "name": "market",
          "address": "7xf68VEvMtLJ2Up5zVZdA161uH1yNkTFYgDFhNUSwkM7"
        }
      ],
      "args": []
    },
    {
      "name": "beginLegacyRetirement",
      "discriminator": [
        190,
        149,
        210,
        81,
        14,
        75,
        16,
        56
      ],
      "accounts": [
        {
          "name": "authority",
          "writable": true,
          "signer": true,
          "address": "EdYBBhu2PU84eNt6r8zPWkaxrxbXVAzVvAz3tR3XveRq"
        },
        {
          "name": "programData",
          "address": "58HpCNM5jydkig5bhy8BR77DJn2E3KcuD9hruj13mUqz"
        },
        {
          "name": "frozen",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  103,
                  97,
                  99,
                  121,
                  95,
                  102,
                  114,
                  101,
                  101,
                  122,
                  101
                ]
              },
              {
                "kind": "const",
                "value": [
                  103,
                  104,
                  33,
                  254,
                  53,
                  66,
                  172,
                  146,
                  108,
                  179,
                  114,
                  56,
                  133,
                  170,
                  139,
                  129,
                  24,
                  11,
                  46,
                  78,
                  178,
                  50,
                  184,
                  47,
                  5,
                  123,
                  200,
                  48,
                  237,
                  243,
                  13,
                  186
                ]
              }
            ]
          }
        },
        {
          "name": "market",
          "writable": true,
          "address": "7xf68VEvMtLJ2Up5zVZdA161uH1yNkTFYgDFhNUSwkM7"
        },
        {
          "name": "retirement",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  103,
                  97,
                  99,
                  121,
                  95,
                  114,
                  101,
                  116,
                  105,
                  114,
                  101,
                  109,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "const",
                "value": [
                  103,
                  104,
                  33,
                  254,
                  53,
                  66,
                  172,
                  146,
                  108,
                  179,
                  114,
                  56,
                  133,
                  170,
                  139,
                  129,
                  24,
                  11,
                  46,
                  78,
                  178,
                  50,
                  184,
                  47,
                  5,
                  123,
                  200,
                  48,
                  237,
                  243,
                  13,
                  186
                ]
              }
            ]
          }
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "eventAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  95,
                  95,
                  101,
                  118,
                  101,
                  110,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104,
                  111,
                  114,
                  105,
                  116,
                  121
                ]
              }
            ]
          }
        },
        {
          "name": "program"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "beginLegacyRetirementArgs"
            }
          }
        }
      ]
    },
    {
      "name": "clearLegacyOwner",
      "discriminator": [
        192,
        68,
        133,
        192,
        119,
        18,
        111,
        162
      ],
      "accounts": [
        {
          "name": "retirement",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  103,
                  97,
                  99,
                  121,
                  95,
                  114,
                  101,
                  116,
                  105,
                  114,
                  101,
                  109,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "const",
                "value": [
                  103,
                  104,
                  33,
                  254,
                  53,
                  66,
                  172,
                  146,
                  108,
                  179,
                  114,
                  56,
                  133,
                  170,
                  139,
                  129,
                  24,
                  11,
                  46,
                  78,
                  178,
                  50,
                  184,
                  47,
                  5,
                  123,
                  200,
                  48,
                  237,
                  243,
                  13,
                  186
                ]
              }
            ]
          },
          "relations": [
            "market"
          ]
        },
        {
          "name": "market",
          "address": "7xf68VEvMtLJ2Up5zVZdA161uH1yNkTFYgDFhNUSwkM7"
        },
        {
          "name": "owner",
          "writable": true
        },
        {
          "name": "eventAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  95,
                  95,
                  101,
                  118,
                  101,
                  110,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104,
                  111,
                  114,
                  105,
                  116,
                  121
                ]
              }
            ]
          }
        },
        {
          "name": "program"
        }
      ],
      "args": [
        {
          "name": "ownerIndex",
          "type": "u8"
        },
        {
          "name": "proof",
          "type": {
            "vec": {
              "array": [
                "u8",
                32
              ]
            }
          }
        }
      ]
    },
    {
      "name": "finishLegacyRetirement",
      "discriminator": [
        161,
        141,
        196,
        254,
        198,
        1,
        75,
        245
      ],
      "accounts": [
        {
          "name": "retirement",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  103,
                  97,
                  99,
                  121,
                  95,
                  114,
                  101,
                  116,
                  105,
                  114,
                  101,
                  109,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "const",
                "value": [
                  103,
                  104,
                  33,
                  254,
                  53,
                  66,
                  172,
                  146,
                  108,
                  179,
                  114,
                  56,
                  133,
                  170,
                  139,
                  129,
                  24,
                  11,
                  46,
                  78,
                  178,
                  50,
                  184,
                  47,
                  5,
                  123,
                  200,
                  48,
                  237,
                  243,
                  13,
                  186
                ]
              }
            ]
          },
          "relations": [
            "market"
          ]
        },
        {
          "name": "market",
          "address": "7xf68VEvMtLJ2Up5zVZdA161uH1yNkTFYgDFhNUSwkM7"
        },
        {
          "name": "baseVault",
          "writable": true
        },
        {
          "name": "quoteVault",
          "writable": true
        },
        {
          "name": "baseMint"
        },
        {
          "name": "quoteMint"
        },
        {
          "name": "baseRecipient",
          "writable": true
        },
        {
          "name": "quoteRecipient",
          "writable": true
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "eventAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  95,
                  95,
                  101,
                  118,
                  101,
                  110,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104,
                  111,
                  114,
                  105,
                  116,
                  121
                ]
              }
            ]
          }
        },
        {
          "name": "program"
        }
      ],
      "args": []
    },
    {
      "name": "freezeLegacyMarket",
      "discriminator": [
        204,
        89,
        51,
        123,
        254,
        109,
        57,
        250
      ],
      "accounts": [
        {
          "name": "authority",
          "writable": true,
          "signer": true,
          "address": "EdYBBhu2PU84eNt6r8zPWkaxrxbXVAzVvAz3tR3XveRq"
        },
        {
          "name": "programData",
          "address": "58HpCNM5jydkig5bhy8BR77DJn2E3KcuD9hruj13mUqz"
        },
        {
          "name": "market",
          "address": "7xf68VEvMtLJ2Up5zVZdA161uH1yNkTFYgDFhNUSwkM7"
        },
        {
          "name": "frozen",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  103,
                  97,
                  99,
                  121,
                  95,
                  102,
                  114,
                  101,
                  101,
                  122,
                  101
                ]
              },
              {
                "kind": "const",
                "value": [
                  103,
                  104,
                  33,
                  254,
                  53,
                  66,
                  172,
                  146,
                  108,
                  179,
                  114,
                  56,
                  133,
                  170,
                  139,
                  129,
                  24,
                  11,
                  46,
                  78,
                  178,
                  50,
                  184,
                  47,
                  5,
                  123,
                  200,
                  48,
                  237,
                  243,
                  13,
                  186
                ]
              }
            ]
          }
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        },
        {
          "name": "eventAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  95,
                  95,
                  101,
                  118,
                  101,
                  110,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104,
                  111,
                  114,
                  105,
                  116,
                  121
                ]
              }
            ]
          }
        },
        {
          "name": "program"
        }
      ],
      "args": []
    },
    {
      "name": "payLegacyOwner",
      "discriminator": [
        176,
        241,
        249,
        80,
        48,
        55,
        226,
        75
      ],
      "accounts": [
        {
          "name": "retirement",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  103,
                  97,
                  99,
                  121,
                  95,
                  114,
                  101,
                  116,
                  105,
                  114,
                  101,
                  109,
                  101,
                  110,
                  116
                ]
              },
              {
                "kind": "const",
                "value": [
                  103,
                  104,
                  33,
                  254,
                  53,
                  66,
                  172,
                  146,
                  108,
                  179,
                  114,
                  56,
                  133,
                  170,
                  139,
                  129,
                  24,
                  11,
                  46,
                  78,
                  178,
                  50,
                  184,
                  47,
                  5,
                  123,
                  200,
                  48,
                  237,
                  243,
                  13,
                  186
                ]
              }
            ]
          },
          "relations": [
            "market"
          ]
        },
        {
          "name": "market",
          "address": "7xf68VEvMtLJ2Up5zVZdA161uH1yNkTFYgDFhNUSwkM7"
        },
        {
          "name": "baseVault",
          "writable": true
        },
        {
          "name": "quoteVault",
          "writable": true
        },
        {
          "name": "baseMint"
        },
        {
          "name": "quoteMint"
        },
        {
          "name": "baseRecipient",
          "writable": true
        },
        {
          "name": "quoteRecipient",
          "writable": true
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "eventAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  95,
                  95,
                  101,
                  118,
                  101,
                  110,
                  116,
                  95,
                  97,
                  117,
                  116,
                  104,
                  111,
                  114,
                  105,
                  116,
                  121
                ]
              }
            ]
          }
        },
        {
          "name": "program"
        }
      ],
      "args": [
        {
          "name": "ownerIndex",
          "type": "u8"
        }
      ]
    }
  ],
  "accounts": [
    {
      "name": "frozenLegacyMarket",
      "discriminator": [
        9,
        39,
        150,
        81,
        233,
        202,
        121,
        57
      ]
    },
    {
      "name": "legacyRetirement",
      "discriminator": [
        206,
        112,
        210,
        10,
        111,
        175,
        42,
        99
      ]
    },
    {
      "name": "retiredLegacyMarket",
      "discriminator": [
        145,
        235,
        18,
        42,
        106,
        110,
        24,
        236
      ]
    }
  ],
  "events": [
    {
      "name": "legacyMarketFrozen",
      "discriminator": [
        107,
        227,
        120,
        110,
        157,
        70,
        63,
        1
      ]
    },
    {
      "name": "legacyOwnerCleared",
      "discriminator": [
        253,
        104,
        223,
        44,
        75,
        234,
        112,
        85
      ]
    },
    {
      "name": "legacyRetirementBegun",
      "discriminator": [
        124,
        37,
        125,
        184,
        174,
        244,
        30,
        148
      ]
    },
    {
      "name": "legacyRetirementPaid",
      "discriminator": [
        31,
        226,
        86,
        140,
        165,
        183,
        1,
        196
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "authority",
      "msg": "Retirement authority differs from the reviewed scope"
    },
    {
      "code": 6001,
      "name": "scope",
      "msg": "Account or owner differs from the fixed legacy retirement scope"
    },
    {
      "code": 6002,
      "name": "evidence",
      "msg": "Retirement evidence differs from the frozen snapshot"
    },
    {
      "code": 6003,
      "name": "funding",
      "msg": "Reviewed custody cannot fund the immutable retirement claims"
    },
    {
      "code": 6004,
      "name": "minimum",
      "msg": "Net entitlement is below its reviewed 99% minimum"
    },
    {
      "code": 6005,
      "name": "recipient",
      "msg": "Unsafe or incorrect retirement recipient"
    },
    {
      "code": 6006,
      "name": "unsettled",
      "msg": "All personal claims must be cleared before terminal payouts"
    },
    {
      "code": 6007,
      "name": "repeated",
      "msg": "Retirement action has already completed"
    },
    {
      "code": 6008,
      "name": "math",
      "msg": "Retirement arithmetic overflow"
    }
  ],
  "types": [
    {
      "name": "beginLegacyRetirementArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "sourceSlot",
            "type": "u64"
          },
          {
            "name": "bankSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "quoteSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "ownerEvidenceRoot",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "payouts",
            "type": {
              "array": [
                {
                  "defined": {
                    "name": "legacyPayout"
                  }
                },
                6
              ]
            }
          }
        ]
      }
    },
    {
      "name": "frozenLegacyMarket",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "marketSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "clockBytes",
            "type": {
              "array": [
                "u8",
                40
              ]
            }
          },
          {
            "name": "vaultAmounts",
            "type": {
              "array": [
                "u64",
                8
              ]
            }
          },
          {
            "name": "receiptSnapshot",
            "type": "bytes"
          }
        ]
      }
    },
    {
      "name": "legacyMarketFrozen",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "slot",
            "type": "u64"
          },
          {
            "name": "marketSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "receiptsSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          }
        ]
      }
    },
    {
      "name": "legacyOwnerCleared",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "rentLamports",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "legacyPayout",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "quotedBase",
            "type": "u64"
          },
          {
            "name": "quotedQuote",
            "type": "u64"
          },
          {
            "name": "base",
            "type": "u64"
          },
          {
            "name": "quote",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "legacyRetirement",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "version",
            "type": "u8"
          },
          {
            "name": "bump",
            "type": "u8"
          },
          {
            "name": "completed",
            "type": "bool"
          },
          {
            "name": "cleanedMask",
            "type": "u8"
          },
          {
            "name": "paidMask",
            "type": "u8"
          },
          {
            "name": "sourceSlot",
            "type": "u64"
          },
          {
            "name": "manifestSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "bankSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "quoteSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "ownerEvidenceRoot",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "payouts",
            "type": {
              "array": [
                {
                  "defined": {
                    "name": "legacyPayout"
                  }
                },
                6
              ]
            }
          },
          {
            "name": "custody",
            "type": {
              "array": [
                "u64",
                2
              ]
            }
          },
          {
            "name": "remaining",
            "type": {
              "array": [
                "u64",
                2
              ]
            }
          }
        ]
      }
    },
    {
      "name": "legacyRetirementBegun",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "manifestSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "sourceSlot",
            "type": "u64"
          },
          {
            "name": "custody",
            "type": {
              "array": [
                "u64",
                2
              ]
            }
          }
        ]
      }
    },
    {
      "name": "legacyRetirementPaid",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "base",
            "type": "u64"
          },
          {
            "name": "quote",
            "type": "u64"
          },
          {
            "name": "treasury",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "retiredLegacyMarket",
      "docs": [
        "Permanent replay/reinitialization barrier. Neither this account nor the",
        "settlement state has a close instruction, including after final payment."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "retirement",
            "type": "pubkey"
          },
          {
            "name": "version",
            "type": "u8"
          }
        ]
      }
    }
  ]
};
