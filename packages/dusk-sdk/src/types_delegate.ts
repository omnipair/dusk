/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/leverage_delegate.json`.
 */
export type LeverageDelegate = {
  "address": "AXNfmZt5e1UM4daeTzW3H7zNo4boobBcnFm8RzJYxvAv",
  "metadata": {
    "name": "leverageDelegate",
    "version": "2.0.0",
    "spec": "0.1.0",
    "description": "Permissionless leverage delegation strategies for Omnipair V2 (Dusk)"
  },
  "instructions": [
    {
      "name": "afterCloseOrder",
      "discriminator": [
        156,
        224,
        238,
        250,
        95,
        229,
        235,
        59
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "order.position",
                "account": "leverageOrder"
              },
              {
                "kind": "account",
                "path": "order.owner",
                "account": "leverageOrder"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true
        },
        {
          "name": "leveragePosition"
        },
        {
          "name": "leverageDelegation"
        },
        {
          "name": "custodyTokenAccount",
          "writable": true
        },
        {
          "name": "executorTokenAccount",
          "writable": true
        },
        {
          "name": "ownerTokenAccount",
          "writable": true
        },
        {
          "name": "tokenMint"
        },
        {
          "name": "executor",
          "signer": true
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "executeOrderArgs"
            }
          }
        }
      ]
    },
    {
      "name": "beforeStopLoss",
      "discriminator": [
        246,
        62,
        82,
        232,
        168,
        199,
        121,
        78
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "leveragePosition"
              },
              {
                "kind": "account",
                "path": "order.owner",
                "account": "leverageOrder"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "market"
        },
        {
          "name": "leveragePosition"
        },
        {
          "name": "leverageDelegation"
        },
        {
          "name": "custodyTokenAccount"
        },
        {
          "name": "collateralMint",
          "docs": [
            "Collateral mint is needed to reproduce the exact net reserve credit for",
            "Token-2022 transfer-fee assets before approving a partial close."
          ]
        },
        {
          "name": "tokenMint"
        },
        {
          "name": "executor",
          "signer": true
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "executeOrderArgs"
            }
          }
        }
      ]
    },
    {
      "name": "beforeTakeProfit",
      "discriminator": [
        5,
        35,
        111,
        0,
        223,
        131,
        193,
        31
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "leveragePosition"
              },
              {
                "kind": "account",
                "path": "order.owner",
                "account": "leverageOrder"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "market"
        },
        {
          "name": "leveragePosition"
        },
        {
          "name": "leverageDelegation"
        },
        {
          "name": "custodyTokenAccount"
        },
        {
          "name": "collateralMint",
          "docs": [
            "Collateral mint is needed to reproduce the exact net reserve credit for",
            "Token-2022 transfer-fee assets before approving a partial close."
          ]
        },
        {
          "name": "tokenMint"
        },
        {
          "name": "executor",
          "signer": true
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "executeOrderArgs"
            }
          }
        }
      ]
    },
    {
      "name": "cancelHlpOrder",
      "discriminator": [
        129,
        178,
        88,
        77,
        244,
        39,
        12,
        194
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  104,
                  108,
                  112,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "order.market",
                "account": "hlpOrder"
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "account",
                "path": "order.target_hlp_mint",
                "account": "hlpOrder"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "targetHlpMint"
        },
        {
          "name": "custodyHlpAccount",
          "writable": true
        },
        {
          "name": "ownerHlpAccount",
          "writable": true
        },
        {
          "name": "owner",
          "signer": true
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "hlpOrderIdArgs"
            }
          }
        }
      ]
    },
    {
      "name": "cancelLeverageEntryOrder",
      "discriminator": [
        171,
        34,
        4,
        86,
        45,
        211,
        119,
        82
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  101,
                  110,
                  116,
                  114,
                  121,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "order.market",
                "account": "leverageEntryOrder"
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "debtMint"
        },
        {
          "name": "fundingVault",
          "writable": true
        },
        {
          "name": "ownerFundingAccount",
          "writable": true
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "leverageEntryOrderIdArgs"
            }
          }
        }
      ]
    },
    {
      "name": "cancelLeverageOrder",
      "discriminator": [
        26,
        88,
        173,
        106,
        175,
        242,
        203,
        122
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "order.position",
                "account": "leverageOrder"
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "cancelLeverageOrderArgs"
            }
          }
        }
      ]
    },
    {
      "name": "createHlpOrder",
      "discriminator": [
        65,
        56,
        157,
        186,
        107,
        208,
        125,
        163
      ],
      "accounts": [
        {
          "name": "market"
        },
        {
          "name": "targetHlpMint"
        },
        {
          "name": "baseMint"
        },
        {
          "name": "quoteMint"
        },
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  104,
                  108,
                  112,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "account",
                "path": "targetHlpMint"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "ownerHlpAccount",
          "writable": true
        },
        {
          "name": "custodyHlpAccount",
          "writable": true
        },
        {
          "name": "baseYieldAccount",
          "writable": true
        },
        {
          "name": "quoteYieldAccount",
          "writable": true
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "duskEventAuthority",
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
            ],
            "program": {
              "kind": "const",
              "value": [
                254,
                237,
                118,
                109,
                5,
                146,
                245,
                249,
                66,
                135,
                243,
                124,
                36,
                53,
                12,
                19,
                89,
                72,
                84,
                7,
                236,
                95,
                227,
                238,
                53,
                42,
                79,
                224,
                225,
                53,
                141,
                56
              ]
            }
          }
        },
        {
          "name": "duskProgram",
          "address": "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "createHlpOrderArgs"
            }
          }
        }
      ]
    },
    {
      "name": "createLeverageEntryOrder",
      "discriminator": [
        142,
        234,
        116,
        175,
        25,
        208,
        151,
        23
      ],
      "accounts": [
        {
          "name": "market"
        },
        {
          "name": "debtMint"
        },
        {
          "name": "collateralMint"
        },
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  101,
                  110,
                  116,
                  114,
                  121,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "ownerFundingAccount",
          "writable": true
        },
        {
          "name": "fundingVault",
          "writable": true
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "createLeverageEntryOrderArgs"
            }
          }
        }
      ]
    },
    {
      "name": "createLeverageOrder",
      "discriminator": [
        197,
        206,
        10,
        223,
        89,
        46,
        93,
        17
      ],
      "accounts": [
        {
          "name": "market"
        },
        {
          "name": "leveragePosition"
        },
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "leveragePosition"
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "createLeverageOrderArgs"
            }
          }
        }
      ]
    },
    {
      "name": "executeHlpOrder",
      "discriminator": [
        186,
        50,
        177,
        108,
        168,
        202,
        142,
        201
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  104,
                  108,
                  112,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "order.owner",
                "account": "hlpOrder"
              },
              {
                "kind": "account",
                "path": "order.target_hlp_mint",
                "account": "hlpOrder"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "baseMint"
        },
        {
          "name": "quoteMint"
        },
        {
          "name": "ylpMint",
          "writable": true
        },
        {
          "name": "targetHlpMint",
          "writable": true
        },
        {
          "name": "baseReserveVault",
          "writable": true
        },
        {
          "name": "quoteReserveVault",
          "writable": true
        },
        {
          "name": "borrowedInterestVault",
          "writable": true
        },
        {
          "name": "custodyTargetAccount",
          "writable": true
        },
        {
          "name": "custodyHlpAccount",
          "writable": true
        },
        {
          "name": "hlpYlpAccount",
          "writable": true
        },
        {
          "name": "baseYieldAccount",
          "writable": true
        },
        {
          "name": "quoteYieldAccount",
          "writable": true
        },
        {
          "name": "ownerTargetAccount",
          "writable": true
        },
        {
          "name": "executorTargetAccount",
          "writable": true
        },
        {
          "name": "executor",
          "signer": true
        },
        {
          "name": "orderOwner",
          "writable": true
        },
        {
          "name": "duskEventAuthority",
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
            ],
            "program": {
              "kind": "const",
              "value": [
                254,
                237,
                118,
                109,
                5,
                146,
                245,
                249,
                66,
                135,
                243,
                124,
                36,
                53,
                12,
                19,
                89,
                72,
                84,
                7,
                236,
                95,
                227,
                238,
                53,
                42,
                79,
                224,
                225,
                53,
                141,
                56
              ]
            }
          }
        },
        {
          "name": "duskProgram",
          "address": "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X"
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "hlpOrderIdArgs"
            }
          }
        }
      ]
    },
    {
      "name": "executeLeverageEntryOrder",
      "discriminator": [
        99,
        67,
        57,
        97,
        204,
        71,
        172,
        144
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  101,
                  110,
                  116,
                  114,
                  121,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "order.owner",
                "account": "leverageEntryOrder"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "owner",
          "writable": true
        },
        {
          "name": "leveragePosition",
          "writable": true
        },
        {
          "name": "debtMint"
        },
        {
          "name": "collateralMint"
        },
        {
          "name": "debtReserveVault",
          "writable": true
        },
        {
          "name": "collateralReserveVault",
          "writable": true
        },
        {
          "name": "leverageCollateralVault",
          "writable": true
        },
        {
          "name": "fundingVault",
          "writable": true
        },
        {
          "name": "ownerRefundAccount",
          "writable": true
        },
        {
          "name": "executorBountyAccount",
          "writable": true
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "optional": true
        },
        {
          "name": "instructionsSysvar",
          "address": "Sysvar1nstructions1111111111111111111111111"
        },
        {
          "name": "executor",
          "writable": true,
          "signer": true
        },
        {
          "name": "duskEventAuthority",
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
            ],
            "program": {
              "kind": "const",
              "value": [
                254,
                237,
                118,
                109,
                5,
                146,
                245,
                249,
                66,
                135,
                243,
                124,
                36,
                53,
                12,
                19,
                89,
                72,
                84,
                7,
                236,
                95,
                227,
                238,
                53,
                42,
                79,
                224,
                225,
                53,
                141,
                56
              ]
            }
          }
        },
        {
          "name": "duskProgram",
          "address": "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X"
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "leverageEntryOrderIdArgs"
            }
          }
        }
      ]
    },
    {
      "name": "settleHlpOrderYield",
      "discriminator": [
        1,
        104,
        193,
        13,
        13,
        9,
        243,
        151
      ],
      "accounts": [
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  104,
                  108,
                  112,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "order.owner",
                "account": "hlpOrder"
              },
              {
                "kind": "account",
                "path": "order.target_hlp_mint",
                "account": "hlpOrder"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "targetHlpMint"
        },
        {
          "name": "custodyHlpAccount",
          "writable": true
        },
        {
          "name": "ownerHlpAccount",
          "writable": true
        },
        {
          "name": "baseMint"
        },
        {
          "name": "quoteMint"
        },
        {
          "name": "baseReserveVault",
          "writable": true
        },
        {
          "name": "quoteReserveVault",
          "writable": true
        },
        {
          "name": "baseInterestVault",
          "writable": true
        },
        {
          "name": "quoteInterestVault",
          "writable": true
        },
        {
          "name": "baseYieldAccount",
          "writable": true
        },
        {
          "name": "quoteYieldAccount",
          "writable": true
        },
        {
          "name": "ownerBaseAccount",
          "writable": true
        },
        {
          "name": "ownerQuoteAccount",
          "writable": true
        },
        {
          "name": "owner",
          "writable": true
        },
        {
          "name": "duskEventAuthority",
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
            ],
            "program": {
              "kind": "const",
              "value": [
                254,
                237,
                118,
                109,
                5,
                146,
                245,
                249,
                66,
                135,
                243,
                124,
                36,
                53,
                12,
                19,
                89,
                72,
                84,
                7,
                236,
                95,
                227,
                238,
                53,
                42,
                79,
                224,
                225,
                53,
                141,
                56
              ]
            }
          }
        },
        {
          "name": "duskProgram",
          "address": "JA8Zxxm4t4zopBL8e3dQQXWfQ3a5pBUPY9Sp9RnybV2X"
        },
        {
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "hlpOrderIdArgs"
            }
          }
        }
      ]
    },
    {
      "name": "updateLeverageOrder",
      "discriminator": [
        25,
        101,
        101,
        3,
        125,
        229,
        46,
        242
      ],
      "accounts": [
        {
          "name": "market"
        },
        {
          "name": "leveragePosition"
        },
        {
          "name": "order",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  108,
                  101,
                  118,
                  101,
                  114,
                  97,
                  103,
                  101,
                  95,
                  111,
                  114,
                  100,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "leveragePosition"
              },
              {
                "kind": "account",
                "path": "owner"
              },
              {
                "kind": "arg",
                "path": "args.order_id"
              }
            ]
          }
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "updateLeverageOrderArgs"
            }
          }
        }
      ]
    }
  ],
  "accounts": [
    {
      "name": "futarchyAuthority",
      "discriminator": [
        175,
        247,
        160,
        182,
        140,
        128,
        211,
        226
      ]
    },
    {
      "name": "hlpOrder",
      "discriminator": [
        162,
        69,
        68,
        12,
        45,
        8,
        128,
        211
      ]
    },
    {
      "name": "leverageDelegation",
      "discriminator": [
        49,
        60,
        29,
        23,
        243,
        219,
        16,
        214
      ]
    },
    {
      "name": "leverageEntryOrder",
      "discriminator": [
        48,
        210,
        138,
        201,
        116,
        174,
        237,
        159
      ]
    },
    {
      "name": "leverageOrder",
      "discriminator": [
        232,
        162,
        45,
        148,
        106,
        106,
        37,
        132
      ]
    },
    {
      "name": "leveragePosition",
      "discriminator": [
        88,
        78,
        124,
        68,
        228,
        129,
        34,
        251
      ]
    },
    {
      "name": "market",
      "discriminator": [
        219,
        190,
        213,
        55,
        0,
        227,
        198,
        154
      ]
    },
    {
      "name": "referralAccrual",
      "discriminator": [
        35,
        246,
        25,
        66,
        174,
        160,
        48,
        39
      ]
    },
    {
      "name": "referralPartner",
      "discriminator": [
        234,
        54,
        169,
        157,
        142,
        187,
        225,
        214
      ]
    },
    {
      "name": "yieldAccount",
      "discriminator": [
        233,
        241,
        119,
        6,
        2,
        14,
        106,
        156
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "invalidOrder",
      "msg": "Invalid leverage order"
    },
    {
      "code": 6001,
      "name": "triggerNotMet",
      "msg": "Order trigger is not met"
    },
    {
      "code": 6002,
      "name": "invalidTokenAccount",
      "msg": "Invalid token account"
    },
    {
      "code": 6003,
      "name": "mathOverflow",
      "msg": "Math overflow"
    },
    {
      "code": 6004,
      "name": "approvalSerializationFailed",
      "msg": "Approval serialization failed"
    },
    {
      "code": 6005,
      "name": "invalidMarketVersion",
      "msg": "Unsupported Dusk market version"
    }
  ],
  "types": [
    {
      "name": "ammConfig",
      "docs": [
        "AMM controls. One-times peak amplification with zero widths selects the",
        "full-range CPMM branch of the same concentrated implementation."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "peakAmplificationNad",
            "type": "u64"
          },
          {
            "name": "coreHalfWidthBps",
            "type": "u16"
          },
          {
            "name": "fadeWidthBps",
            "type": "u16"
          },
          {
            "name": "centerEmaHalfLifeMs",
            "type": "u64"
          },
          {
            "name": "volatilityHalfLifeMs",
            "type": "u64"
          },
          {
            "name": "adjustmentThresholdNad",
            "type": "u64"
          },
          {
            "name": "adjustmentStepNad",
            "type": "u64"
          },
          {
            "name": "minAdjustmentIntervalSlots",
            "type": "u64"
          },
          {
            "name": "volatilityShockCapNad",
            "type": "u64"
          },
          {
            "name": "volatilityCapNad",
            "type": "u64"
          },
          {
            "name": "divergenceFeeCoefficientNad",
            "type": "u64"
          },
          {
            "name": "volatilityFeeCoefficientNad",
            "type": "u64"
          },
          {
            "name": "swapFeeCollectMode",
            "docs": [
              "Asset in which swap, toxicity, volatility, and retained-recenter fees",
              "are denominated. Lending and hLP funding interest remain in the",
              "borrowed asset and are not affected by this setting."
            ],
            "type": "u8"
          },
          {
            "name": "compoundingFeeBps",
            "docs": [
              "Share of the LP-owned swap fee which becomes ordinary reserve",
              "principal instead of a claimable fee liability. Zero disables native",
              "compounding; `BPS_DENOMINATOR` compounds the complete LP share."
            ],
            "type": "u16"
          },
          {
            "name": "launchFeeStartBps",
            "docs": [
              "Optional launch-only base fee. The premium above `swap_fee_bps`",
              "decays from `start_time` and is zero after the configured duration."
            ],
            "type": "u16"
          },
          {
            "name": "launchFeeDurationSeconds",
            "type": "u64"
          },
          {
            "name": "launchFeeDecayMode",
            "type": "u8"
          },
          {
            "name": "launchMarketPriceStepBps",
            "docs": [
              "When all three values are zero, the launch fee follows the time",
              "schedule above. A fully nonzero tuple selects a price-milestone",
              "scheduler whose reference price is bound by the first liquidity seed."
            ],
            "type": "u16"
          },
          {
            "name": "launchMarketNumberOfPeriods",
            "type": "u16"
          },
          {
            "name": "launchMarketReductionFactorBps",
            "type": "u16"
          },
          {
            "name": "launchRateLimitAsset",
            "docs": [
              "Optional launch buy-size limiter. The configured asset is the asset",
              "being bought, not the input asset. Each full/partial reference amount",
              "after the first adds `launch_rate_limit_increment_bps`, capped by",
              "`launch_rate_limit_max_fee_bps`."
            ],
            "type": "u8"
          },
          {
            "name": "launchRateLimitReferenceNad",
            "type": "u64"
          },
          {
            "name": "launchRateLimitIncrementBps",
            "type": "u16"
          },
          {
            "name": "launchRateLimitMaxFeeBps",
            "type": "u16"
          },
          {
            "name": "launchRateLimitDurationSeconds",
            "type": "u64"
          },
          {
            "name": "reserved",
            "type": {
              "array": [
                "u8",
                0
              ]
            }
          }
        ]
      }
    },
    {
      "name": "ammState",
      "docs": [
        "Embedded mutable state for the concentrated curve, internal signals, and",
        "protected recenter liquidity."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "initialized",
            "type": "bool"
          },
          {
            "name": "concentratedCurveCache",
            "docs": [
              "Concentrated CPMM-tail/band geometry. CPMM is represented by zero",
              "concentrated liquidity in this same cache."
            ],
            "type": {
              "defined": {
                "name": "concentratedCurveCache"
              }
            }
          },
          {
            "name": "centerPriceNad",
            "type": "u64"
          },
          {
            "name": "priceEmaNad",
            "type": "u64"
          },
          {
            "name": "lastTradePriceNad",
            "type": "u64"
          },
          {
            "name": "lastObservationSlot",
            "type": "u64"
          },
          {
            "name": "lastAdjustmentSlot",
            "type": "u64"
          },
          {
            "name": "launchReferencePriceNad",
            "docs": [
              "Immutable launch price reference bound by the first fully-backed",
              "liquidity seed. Zero is allowed only before that seed."
            ],
            "type": "u64"
          },
          {
            "name": "launchFeeProgressOffset",
            "docs": [
              "Fee-schedule progress already completed by a bootstrap adapter before",
              "the market graduates into GAMM."
            ],
            "type": "u16"
          },
          {
            "name": "volatilityAccumulatorNad",
            "type": "u64"
          },
          {
            "name": "curveDepthPerShareNad",
            "type": "u128"
          },
          {
            "name": "protectedFloorPerShareNad",
            "docs": [
              "yLP principal floor protected from funded recenter/ramp impairment."
            ],
            "type": "u128"
          },
          {
            "name": "retentionRequiredNad",
            "docs": [
              "Fresh protected-profit target that arms retained surcharge routing.",
              "This is a principal-budget target, never a cap on trader fees."
            ],
            "type": "u128"
          },
          {
            "name": "retentionStopNad",
            "docs": [
              "Hysteresis threshold below which retention remains armed."
            ],
            "type": "u128"
          },
          {
            "name": "retentionHardCapNad",
            "docs": [
              "Maximum protected principal one controller target may request/spend.",
              "It does not clip divergence or volatility surcharge amounts."
            ],
            "type": "u128"
          },
          {
            "name": "retainDynamicSurcharge",
            "docs": [
              "When true, dynamic surcharge is locked in the non-quoteable protected",
              "recenter bucket; when false, the identical trader charge is routed to",
              "claimable yLP fee accounting."
            ],
            "type": "bool"
          },
          {
            "name": "retentionTargetSaturated",
            "docs": [
              "The requested protection target exceeded its principal-budget cap."
            ],
            "type": "bool"
          },
          {
            "name": "retentionTargetStale",
            "docs": [
              "The protected bucket changed after the last exact forward-target solve.",
              "While stale, retention stays on until a decision point refreshes the",
              "target or executes a funded recenter."
            ],
            "type": "bool"
          },
          {
            "name": "deferredControllerTarget",
            "docs": [
              "Exact unfunded controller target retried by later real operations."
            ],
            "type": {
              "defined": {
                "name": "deferredControllerTarget"
              }
            }
          },
          {
            "name": "reserved",
            "type": {
              "array": [
                "u8",
                0
              ]
            }
          }
        ]
      }
    },
    {
      "name": "cancelLeverageOrderArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "orderId",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "concentratedCurveCache",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "mathRevision",
            "type": "u8"
          },
          {
            "name": "peakAmplificationNad",
            "type": "u64"
          },
          {
            "name": "coreHalfWidthBps",
            "type": "u16"
          },
          {
            "name": "fadeWidthBps",
            "type": "u16"
          },
          {
            "name": "tailLiquidity",
            "type": "u128"
          },
          {
            "name": "concentratedLiquidity",
            "type": "u128"
          },
          {
            "name": "coreLowerSqrtPriceNad",
            "type": "u128"
          },
          {
            "name": "coreUpperSqrtPriceNad",
            "type": "u128"
          },
          {
            "name": "outerLowerSqrtPriceNad",
            "type": "u128"
          },
          {
            "name": "outerUpperSqrtPriceNad",
            "type": "u128"
          }
        ]
      }
    },
    {
      "name": "createHlpOrderArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "orderId",
            "type": "u64"
          },
          {
            "name": "kind",
            "type": "u8"
          },
          {
            "name": "hlpAmount",
            "type": "u64"
          },
          {
            "name": "triggerNad",
            "docs": [
              "Stop Loss: principal NAV per hLP token in NAD. Stop Rate: opposite",
              "funding APR in NAD (NAD == 100% APR)."
            ],
            "type": "u64"
          },
          {
            "name": "minTargetAmountOut",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "createLeverageEntryOrderArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "orderId",
            "type": "u64"
          },
          {
            "name": "positionId",
            "type": "pubkey"
          },
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "depositAmount",
            "docs": [
              "Gross amount transferred into escrow. The order records the measured",
              "net credit so Token-2022 transfer fees cannot underfund execution."
            ],
            "type": "u64"
          },
          {
            "name": "minMarginAmount",
            "type": "u64"
          },
          {
            "name": "executorBounty",
            "type": "u64"
          },
          {
            "name": "multiplierBps",
            "type": "u64"
          },
          {
            "name": "limitPriceNad",
            "docs": [
              "Conservative all-in Quote-per-Base execution limit."
            ],
            "type": "u64"
          },
          {
            "name": "minCollateralOut",
            "type": "u64"
          },
          {
            "name": "expiryUnixTimestamp",
            "type": "i64"
          },
          {
            "name": "referrer",
            "type": {
              "option": "pubkey"
            }
          }
        ]
      }
    },
    {
      "name": "createLeverageOrderArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "orderId",
            "type": "u64"
          },
          {
            "name": "kind",
            "type": "u8"
          },
          {
            "name": "triggerCloseoutPriceNad",
            "type": "u64"
          },
          {
            "name": "closeBps",
            "docs": [
              "Portion of the current position closed when triggered. `10_000` is a",
              "full close; smaller values realize one proportional slice."
            ],
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "dailyBorrowBucket",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "borrowedBucket",
            "docs": [
              "Gross principal lent out through the public borrow path. Internal hLP",
              "funding and isolated leverage do not consume this capacity. This is a",
              "24-hour leaky/token bucket, not an exact trailing-window sum: it permits",
              "a full burst after idle and then refills at the configured daily rate."
            ],
            "type": "u64"
          },
          {
            "name": "lastDecaySlot",
            "type": "u64"
          },
          {
            "name": "decayRemainderMs",
            "docs": [
              "Numerator remainder from `limit * elapsed_ms / MS_PER_DAY`. For a fixed",
              "absolute limit, carrying it makes refill independent of how often the",
              "bucket is checkpointed. The bps-derived absolute limit can still move",
              "when conservative market depth changes."
            ],
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "debt",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "fixedBaseShares",
            "type": "u128"
          },
          {
            "name": "fixedQuoteShares",
            "type": "u128"
          },
          {
            "name": "baseBorrowIndexNad",
            "type": "u128"
          },
          {
            "name": "quoteBorrowIndexNad",
            "type": "u128"
          },
          {
            "name": "baseRateAtTargetNad",
            "type": "u128"
          },
          {
            "name": "quoteRateAtTargetNad",
            "type": "u128"
          },
          {
            "name": "globalHealthBaseContributionForQuoteDebt",
            "type": "u64"
          },
          {
            "name": "globalHealthQuoteContributionForBaseDebt",
            "type": "u64"
          },
          {
            "name": "baseLastAccrualSlot",
            "type": "u64"
          },
          {
            "name": "quoteLastAccrualSlot",
            "type": "u64"
          },
          {
            "name": "fixedBasePrincipal",
            "docs": [
              "Aggregate outstanding *principal* (borrowed token amount, excluding",
              "accrued interest) backing fixed margin debt on each side. Accrued",
              "interest is `fixed_*_debt - fixed_*_principal`; tracked so interest can",
              "be routed to the interest vault (non-compounding) instead of",
              "compounding into reserves. Principal is a raw token-atom balance and is",
              "therefore bounded by the corresponding `u64` reserve custody domain."
            ],
            "type": "u64"
          },
          {
            "name": "fixedQuotePrincipal",
            "type": "u64"
          },
          {
            "name": "isolatedBaseShares",
            "docs": [
              "Aggregate isolated leverage debt. This debt contributes to utilization",
              "and interest, but is intentionally not utilized as normal margin debt.",
              "Shares remain `u128`; raw principal remains in the token account's",
              "`u64` amount domain."
            ],
            "type": "u128"
          },
          {
            "name": "isolatedQuoteShares",
            "type": "u128"
          },
          {
            "name": "isolatedBasePrincipal",
            "type": "u64"
          },
          {
            "name": "isolatedQuotePrincipal",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "deferredControllerTarget",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "kind",
            "docs": [
              "0 = none, 2 = center move."
            ],
            "type": "u8"
          },
          {
            "name": "centerPriceNad",
            "type": "u64"
          },
          {
            "name": "requiredNad",
            "type": "u128"
          },
          {
            "name": "evaluatedBaseReserveNad",
            "type": "u128"
          },
          {
            "name": "evaluatedQuoteReserveNad",
            "type": "u128"
          },
          {
            "name": "createdSlot",
            "type": "u64"
          },
          {
            "name": "saturated",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "executeOrderArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "orderId",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "fees",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "swapFeeGrowthIndexQ64",
            "type": "u128"
          },
          {
            "name": "interestGrowthIndexQ64",
            "type": "u128"
          },
          {
            "name": "swapFeeGrowthRemainderScaled",
            "docs": [
              "Scaled fee entitlement not yet representable by the integer growth",
              "index. The corresponding whole-token backing already sits in",
              "`swap_fee_liability`; it must never be redistributed as unallocated",
              "revenue."
            ],
            "type": "u64"
          },
          {
            "name": "interestGrowthRemainderScaled",
            "docs": [
              "Interest counterpart of `swap_fee_growth_remainder_scaled`."
            ],
            "type": "u64"
          },
          {
            "name": "hlpFundingInterestGrowthRemainderScaled",
            "docs": [
              "Source-scoped Q64 carry for interest paid by hLP funding debt. Funding",
              "uses a non-hLP denominator, while public interest uses total yLP",
              "supply; sharing one carry across those denominators would eventually",
              "leak rounding entitlement between the two populations."
            ],
            "type": "u64"
          },
          {
            "name": "swapFeeCustodyBalance",
            "docs": [
              "Claimable swap fees physically held in the reserve vault but excluded",
              "from executable cash and live reserves."
            ],
            "type": "u64"
          },
          {
            "name": "interestVaultBalance",
            "type": "u64"
          },
          {
            "name": "swapFeeLiability",
            "type": "u64"
          },
          {
            "name": "interestLiability",
            "type": "u64"
          },
          {
            "name": "unallocatedSwapFeeLiability",
            "type": "u64"
          },
          {
            "name": "unallocatedInterestLiability",
            "type": "u64"
          },
          {
            "name": "swapProtocolFeeLiability",
            "type": "u64"
          },
          {
            "name": "swapBuybackFeeLiability",
            "type": "u64"
          },
          {
            "name": "interestProtocolFeeLiability",
            "type": "u64"
          },
          {
            "name": "interestBuybackFeeLiability",
            "type": "u64"
          },
          {
            "name": "referralInterestLiability",
            "type": "u64"
          },
          {
            "name": "feeAuctionReferenceMarket",
            "docs": [
              "Governance-approved reference market for fee-lane auctions. A default",
              "key permits only the sold market itself when it directly pairs the sold",
              "and accepted mints."
            ],
            "type": "pubkey"
          },
          {
            "name": "buybackAuctionReferenceMarket",
            "docs": [
              "Governance-approved reference market for buyback-lane auctions. A",
              "default key has the same direct-market-only meaning as above."
            ],
            "type": "pubkey"
          },
          {
            "name": "feeSwapAuctionEpoch",
            "type": {
              "defined": {
                "name": "protocolAuctionEpoch"
              }
            }
          },
          {
            "name": "feeInterestAuctionEpoch",
            "type": {
              "defined": {
                "name": "protocolAuctionEpoch"
              }
            }
          },
          {
            "name": "buybackSwapAuctionEpoch",
            "type": {
              "defined": {
                "name": "protocolAuctionEpoch"
              }
            }
          },
          {
            "name": "buybackInterestAuctionEpoch",
            "type": {
              "defined": {
                "name": "protocolAuctionEpoch"
              }
            }
          }
        ]
      }
    },
    {
      "name": "futarchyAuthority",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "version",
            "type": "u8"
          },
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "recipients",
            "type": {
              "defined": {
                "name": "revenueRecipients"
              }
            }
          },
          {
            "name": "revenueShare",
            "type": {
              "defined": {
                "name": "revenueShare"
              }
            }
          },
          {
            "name": "maxReferralInterestShareBps",
            "type": "u16"
          },
          {
            "name": "revenueDistribution",
            "type": {
              "defined": {
                "name": "revenueDistribution"
              }
            }
          },
          {
            "name": "protocolAuctionSplit",
            "type": {
              "defined": {
                "name": "protocolAuctionSplit"
              }
            }
          },
          {
            "name": "feeAuction",
            "type": {
              "defined": {
                "name": "protocolAuctionConfig"
              }
            }
          },
          {
            "name": "buybackAuction",
            "type": {
              "defined": {
                "name": "protocolAuctionConfig"
              }
            }
          },
          {
            "name": "globalReduceOnly",
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "hlpOrder",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "targetHlpMint",
            "type": "pubkey"
          },
          {
            "name": "custodyHlpAccount",
            "type": "pubkey"
          },
          {
            "name": "orderId",
            "type": "u64"
          },
          {
            "name": "kind",
            "type": "u8"
          },
          {
            "name": "status",
            "type": "u8"
          },
          {
            "name": "hlpAmount",
            "type": "u64"
          },
          {
            "name": "triggerNad",
            "type": "u64"
          },
          {
            "name": "minTargetAmountOut",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "hlpOrderIdArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "orderId",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "hlpVault",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "ylpVault",
            "type": "pubkey"
          },
          {
            "name": "ylpShares",
            "type": "u64"
          },
          {
            "name": "baseHlpLiveReserve",
            "docs": [
              "hLP-owned live reserve depth that is not backed by reserve cash or",
              "normal cash-backed debt. This is the explicit synthetic live component",
              "in `r_virtual = r_cash + r_cash_backed_debt + r_hlp_live`."
            ],
            "type": "u64"
          },
          {
            "name": "quoteHlpLiveReserve",
            "type": "u64"
          },
          {
            "name": "debtShares",
            "docs": [
              "Funding debt used by the hLP vault. It accrues interest and counts",
              "toward utilization, but is not same-side cash-backed reserve debt."
            ],
            "type": "u128"
          },
          {
            "name": "debtPrincipal",
            "docs": [
              "Raw borrowed token atoms; products and indexed shares stay `u128`."
            ],
            "type": "u64"
          },
          {
            "name": "hlpSupply",
            "type": "u64"
          },
          {
            "name": "residualExposure",
            "type": "i128"
          },
          {
            "name": "baseSwapFeeGrowthIndexQ64",
            "type": "u128"
          },
          {
            "name": "baseInterestGrowthIndexQ64",
            "type": "u128"
          },
          {
            "name": "quoteSwapFeeGrowthIndexQ64",
            "type": "u128"
          },
          {
            "name": "quoteInterestGrowthIndexQ64",
            "type": "u128"
          },
          {
            "name": "baseSwapFeeCheckpointQ64",
            "type": "u128"
          },
          {
            "name": "baseInterestCheckpointQ64",
            "type": "u128"
          },
          {
            "name": "quoteSwapFeeCheckpointQ64",
            "type": "u128"
          },
          {
            "name": "quoteInterestCheckpointQ64",
            "type": "u128"
          },
          {
            "name": "baseSwapFeeRemainderQ64",
            "docs": [
              "Aggregate sub-atom yLP entitlement carried across hLP checkpoints.",
              "These are distinct from each holder YieldAccount remainder: this layer",
              "converts vault-owned yLP growth into hLP growth without double-flooring."
            ],
            "type": "u64"
          },
          {
            "name": "baseInterestRemainderQ64",
            "type": "u64"
          },
          {
            "name": "quoteSwapFeeRemainderQ64",
            "type": "u64"
          },
          {
            "name": "quoteInterestRemainderQ64",
            "type": "u64"
          },
          {
            "name": "baseSwapFeeGrowthRemainderScaled",
            "docs": [
              "Sub-index distribution carry for the second, yLP-to-hLP allocation",
              "layer. Whole-token backing represented here has already left the",
              "corresponding `unallocated_*` bucket."
            ],
            "type": "u64"
          },
          {
            "name": "baseInterestGrowthRemainderScaled",
            "type": "u64"
          },
          {
            "name": "quoteSwapFeeGrowthRemainderScaled",
            "type": "u64"
          },
          {
            "name": "quoteInterestGrowthRemainderScaled",
            "type": "u64"
          },
          {
            "name": "unallocatedBaseSwapFeeAmount",
            "type": "u64"
          },
          {
            "name": "unallocatedBaseInterestAmount",
            "type": "u64"
          },
          {
            "name": "unallocatedQuoteSwapFeeAmount",
            "type": "u64"
          },
          {
            "name": "unallocatedQuoteInterestAmount",
            "type": "u64"
          },
          {
            "name": "lastNavNad",
            "type": "u128"
          },
          {
            "name": "cachedSettlementPriceNad",
            "type": "u128"
          },
          {
            "name": "fundingAprEmaNad",
            "docs": [
              "Smoothed APR of the opposite asset borrowed by this target-asset hLP.",
              "The fixed twelve-hour half-life gives Stop Rate orders stable semantics."
            ],
            "type": "u128"
          },
          {
            "name": "fundingAprEmaLastSlot",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "insurance",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "baseVault",
            "type": "pubkey"
          },
          {
            "name": "quoteVault",
            "type": "pubkey"
          },
          {
            "name": "baseAvailable",
            "type": "u64"
          },
          {
            "name": "quoteAvailable",
            "type": "u64"
          },
          {
            "name": "baseDrawWindow",
            "type": {
              "defined": {
                "name": "insuranceDrawWindow"
              }
            }
          },
          {
            "name": "quoteDrawWindow",
            "type": {
              "defined": {
                "name": "insuranceDrawWindow"
              }
            }
          },
          {
            "name": "perEventDrawBps",
            "type": "u16"
          },
          {
            "name": "perDayDrawBps",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "insuranceDrawWindow",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "startSlot",
            "docs": [
              "Slot at which the current 24-hour accounting window began. A zero",
              "value means no draw window has been opened yet."
            ],
            "type": "u64"
          },
          {
            "name": "openingAvailable",
            "docs": [
              "Available insurance at the start of the window, before any draws."
            ],
            "type": "u64"
          },
          {
            "name": "credited",
            "docs": [
              "Net token credits received after the window opened."
            ],
            "type": "u64"
          },
          {
            "name": "drawn",
            "docs": [
              "Gross token amount debited by insurance draws in this window."
            ],
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "irmConfig",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "targetUtilizationBps",
            "type": "u16"
          },
          {
            "name": "curveSteepnessNad",
            "type": "u64"
          },
          {
            "name": "adjustmentSpeedPerYear",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "leverageDelegation",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "delegatedProgram",
            "type": "pubkey"
          },
          {
            "name": "approvedActions",
            "type": "u32"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "leverageEntryOrder",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "positionId",
            "type": "pubkey"
          },
          {
            "name": "debtMint",
            "type": "pubkey"
          },
          {
            "name": "collateralMint",
            "type": "pubkey"
          },
          {
            "name": "orderId",
            "type": "u64"
          },
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "marginAmount",
            "docs": [
              "Gross vault debit forwarded to Dusk as margin."
            ],
            "type": "u64"
          },
          {
            "name": "executorBounty",
            "docs": [
              "Gross vault debit paid to the successful executor."
            ],
            "type": "u64"
          },
          {
            "name": "multiplierBps",
            "type": "u64"
          },
          {
            "name": "limitPriceNad",
            "type": "u64"
          },
          {
            "name": "minCollateralOut",
            "type": "u64"
          },
          {
            "name": "expiryUnixTimestamp",
            "type": "i64"
          },
          {
            "name": "referrer",
            "type": {
              "option": "pubkey"
            }
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "leverageEntryOrderIdArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "orderId",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "leverageOrder",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "orderId",
            "type": "u64"
          },
          {
            "name": "kind",
            "type": "u8"
          },
          {
            "name": "triggerCloseoutPriceNad",
            "type": "u64"
          },
          {
            "name": "closeBps",
            "type": "u16"
          },
          {
            "name": "stagedMargin",
            "type": "u64"
          },
          {
            "name": "stagedCollateralAmount",
            "type": "u64"
          },
          {
            "name": "stagedRemainingCollateralAmount",
            "type": "u64"
          },
          {
            "name": "stagedRemainingDebtShares",
            "type": "u128"
          },
          {
            "name": "stagedRemainingDebtPrincipal",
            "type": "u128"
          },
          {
            "name": "stagedCustodyTokenAccount",
            "type": "pubkey"
          },
          {
            "name": "stagedOutputMint",
            "type": "pubkey"
          },
          {
            "name": "stagedOutputAmount",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "leveragePosition",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "positionId",
            "type": "pubkey"
          },
          {
            "name": "referralPartner",
            "type": "pubkey"
          },
          {
            "name": "referralInterestShareBps",
            "type": "u16"
          },
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "collateralAmount",
            "type": "u64"
          },
          {
            "name": "marginAmount",
            "type": "u64"
          },
          {
            "name": "openNotional",
            "type": "u64"
          },
          {
            "name": "debtPrincipal",
            "type": "u128"
          },
          {
            "name": "debtShares",
            "type": "u128"
          },
          {
            "name": "multiplierBps",
            "type": "u64"
          },
          {
            "name": "openedAt",
            "type": "i64"
          },
          {
            "name": "openedSlot",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "market",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "version",
            "type": "u8"
          },
          {
            "name": "ylpMint",
            "type": "pubkey"
          },
          {
            "name": "baseSide",
            "type": {
              "defined": {
                "name": "marketSide"
              }
            }
          },
          {
            "name": "quoteSide",
            "type": {
              "defined": {
                "name": "marketSide"
              }
            }
          },
          {
            "name": "config",
            "type": {
              "defined": {
                "name": "marketConfig"
              }
            }
          },
          {
            "name": "amm",
            "type": {
              "defined": {
                "name": "ammState"
              }
            }
          },
          {
            "name": "debt",
            "type": {
              "defined": {
                "name": "debt"
              }
            }
          },
          {
            "name": "baseHlpVault",
            "type": {
              "defined": {
                "name": "hlpVault"
              }
            }
          },
          {
            "name": "quoteHlpVault",
            "type": {
              "defined": {
                "name": "hlpVault"
              }
            }
          },
          {
            "name": "risk",
            "type": {
              "defined": {
                "name": "risk"
              }
            }
          },
          {
            "name": "insurance",
            "type": {
              "defined": {
                "name": "insurance"
              }
            }
          },
          {
            "name": "paramsHash",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "initialLiquidityAuthority",
            "docs": [
              "One-shot signer allowed to provide the first fully-backed Base/Quote",
              "seed. It is cleared permanently once yLP supply becomes nonzero."
            ],
            "type": "pubkey"
          },
          {
            "name": "governanceLockedYlp",
            "docs": [
              "External yLP burned into active governance support. This is added back",
              "when computing direct-yLP eligibility; internal reserve-share supply is",
              "intentionally unchanged by governance locking."
            ],
            "type": "u64"
          },
          {
            "name": "parameterRevisions",
            "docs": [
              "Independent monotone revisions for fee, concentration, IRM, EMA,",
              "daily-borrow-limit, and center-controller parameter families."
            ],
            "type": {
              "array": [
                "u64",
                7
              ]
            }
          },
          {
            "name": "lastMarginalObservationNad",
            "docs": [
              "Latest trader-visible marginal price committed by a curve mutation."
            ],
            "type": "u64"
          },
          {
            "name": "curveRevision",
            "docs": [
              "Monotone revision for executable-curve mutations."
            ],
            "type": "u64"
          },
          {
            "name": "riskRevision",
            "docs": [
              "Curve revision represented by the materialized lending-risk snapshot."
            ],
            "type": "u64"
          },
          {
            "name": "lastUpdateSlot",
            "type": "u64"
          },
          {
            "name": "reduceOnly",
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "marketConfig",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "swapFeeBps",
            "type": "u16"
          },
          {
            "name": "divergenceFeeShareCapBps",
            "type": "u16"
          },
          {
            "name": "volatilityFeeShareCapBps",
            "type": "u16"
          },
          {
            "name": "targetHlpLeverageBps",
            "type": "u16"
          },
          {
            "name": "settlementDivergenceBps",
            "type": "u16"
          },
          {
            "name": "emaHalfLifeMs",
            "type": "u64"
          },
          {
            "name": "directionalEmaHalfLifeMs",
            "type": "u64"
          },
          {
            "name": "curveDepthEmaHalfLifeMs",
            "type": "u64"
          },
          {
            "name": "maxDailyBorrowBps",
            "type": "u16"
          },
          {
            "name": "globalHealthContributionCapBps",
            "type": "u16"
          },
          {
            "name": "borrowMarketHealthFloorBps",
            "type": "u16"
          },
          {
            "name": "amm",
            "type": {
              "defined": {
                "name": "ammConfig"
              }
            }
          },
          {
            "name": "irm",
            "type": {
              "defined": {
                "name": "irmConfig"
              }
            }
          },
          {
            "name": "startTime",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "marketSide",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "assetMint",
            "type": "pubkey"
          },
          {
            "name": "assetDecimals",
            "type": "u8"
          },
          {
            "name": "hlpMint",
            "type": "pubkey"
          },
          {
            "name": "reserveVault",
            "type": "pubkey"
          },
          {
            "name": "collateralVault",
            "type": "pubkey"
          },
          {
            "name": "interestVault",
            "type": "pubkey"
          },
          {
            "name": "reserves",
            "type": {
              "defined": {
                "name": "reserves"
              }
            }
          },
          {
            "name": "shares",
            "type": {
              "defined": {
                "name": "reserveShares"
              }
            }
          },
          {
            "name": "fees",
            "type": {
              "defined": {
                "name": "fees"
              }
            }
          },
          {
            "name": "dailyBorrowBucket",
            "type": {
              "defined": {
                "name": "dailyBorrowBucket"
              }
            }
          }
        ]
      }
    },
    {
      "name": "protocolAuctionConfig",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "acceptedMint",
            "type": "pubkey"
          },
          {
            "name": "recipients",
            "type": {
              "defined": {
                "name": "protocolAuctionRecipients"
              }
            }
          },
          {
            "name": "params",
            "type": {
              "defined": {
                "name": "protocolAuctionParams"
              }
            }
          }
        ]
      }
    },
    {
      "name": "protocolAuctionEpoch",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "startSlot",
            "type": "u64"
          },
          {
            "name": "trackedInventory",
            "docs": [
              "Liability remaining immediately after the preceding fill. A larger",
              "current liability proves that new inventory arrived and starts a new",
              "epoch instead of inheriting an old floor price."
            ],
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "protocolAuctionParams",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "startMultiplierBps",
            "type": "u16"
          },
          {
            "name": "floorMultiplierBps",
            "type": "u16"
          },
          {
            "name": "durationSlots",
            "type": "u64"
          },
          {
            "name": "maxReferenceAgeSlots",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "protocolAuctionRecipients",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "treasury",
            "type": "pubkey"
          },
          {
            "name": "stakingVault",
            "type": "pubkey"
          },
          {
            "name": "treasuryBps",
            "type": "u16"
          },
          {
            "name": "stakingVaultBps",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "protocolAuctionSplit",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "feeAuctionBps",
            "type": "u16"
          },
          {
            "name": "buybackAuctionBps",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "referralAccrual",
      "docs": [
        "Claimable referral revenue for one partner, market, and debt asset."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "referralPartner",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "assetMint",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "referralPartner",
      "docs": [
        "A permissioned, protocol-wide referral registry entry."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "recipient",
            "type": "pubkey"
          },
          {
            "name": "interestShareBps",
            "type": "u16"
          },
          {
            "name": "active",
            "type": "bool"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "reserveShares",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "ylpSupply",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "reserves",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "liveReserve",
            "type": "u64"
          },
          {
            "name": "cashReserve",
            "type": "u64"
          },
          {
            "name": "baseHlpBackingInventory",
            "docs": [
              "Physical reserve-vault atoms removed from executable AMM inventory by",
              "base-hLP deleveraging. They are conservation-only bookkeeping, excluded",
              "from hLP NAV and exit output, and return to executable cash pro rata as",
              "base hLP exits."
            ],
            "type": "u64"
          },
          {
            "name": "quoteHlpBackingInventory",
            "docs": [
              "Quote-hLP counterpart of `base_hlp_backing_inventory`; never a second",
              "hLP NAV or withdrawal claim."
            ],
            "type": "u64"
          },
          {
            "name": "protectedRecenterReserve",
            "docs": [
              "Physical reserve-vault atoms retained from toxicity surcharge for a",
              "future protected recenter. They are custody-backed but excluded from",
              "executable cash/live reserves, yLP NAV, and every withdrawal claim."
            ],
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "revenueDistribution",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "futarchyTreasuryBps",
            "type": "u16"
          },
          {
            "name": "buybacksVaultBps",
            "type": "u16"
          },
          {
            "name": "teamTreasuryBps",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "revenueRecipients",
      "docs": [
        "Revenue recipient wallet addresses. Recipient token accounts are derived or",
        "validated against these owners when protocol fees are claimed."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "futarchyTreasury",
            "type": "pubkey"
          },
          {
            "name": "buybacksVault",
            "type": "pubkey"
          },
          {
            "name": "teamTreasury",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "revenueShare",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "swapBps",
            "type": "u16"
          },
          {
            "name": "interestBps",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "risk",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "basePriceEmaNad",
            "type": "u64"
          },
          {
            "name": "quotePriceEmaNad",
            "type": "u64"
          },
          {
            "name": "directionalBasePriceEmaNad",
            "type": "u64"
          },
          {
            "name": "directionalQuotePriceEmaNad",
            "type": "u64"
          },
          {
            "name": "cachedSpotBasePriceNad",
            "type": "u64"
          },
          {
            "name": "cachedSpotQuotePriceNad",
            "type": "u64"
          },
          {
            "name": "observedCurveDepthNad",
            "docs": [
              "Last observed total active curve depth (full-range plus concentrated)."
            ],
            "type": "u128"
          },
          {
            "name": "curveDepthEmaNad",
            "docs": [
              "EMA of total active curve depth."
            ],
            "type": "u128"
          },
          {
            "name": "lastSnapshotSlot",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "updateLeverageOrderArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "orderId",
            "type": "u64"
          },
          {
            "name": "kind",
            "type": "u8"
          },
          {
            "name": "triggerCloseoutPriceNad",
            "type": "u64"
          },
          {
            "name": "closeBps",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "yieldAccount",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "lpMint",
            "docs": [
              "LP mint whose balance earns this account's revenue stream. This keeps",
              "base-hLP, quote-hLP, and yLP entitlements in disjoint PDA namespaces."
            ],
            "type": "pubkey"
          },
          {
            "name": "assetMint",
            "type": "pubkey"
          },
          {
            "name": "tokenKind",
            "type": "u8"
          },
          {
            "name": "recipient",
            "type": "pubkey"
          },
          {
            "name": "swapFeeCheckpointQ64",
            "type": "u128"
          },
          {
            "name": "interestCheckpointQ64",
            "type": "u128"
          },
          {
            "name": "accruedSwapFeeAmount",
            "type": "u64"
          },
          {
            "name": "accruedInterestAmount",
            "type": "u64"
          },
          {
            "name": "swapFeeRemainderQ64",
            "docs": [
              "Sub-token fixed-point entitlement carried across checkpoints. Keeping",
              "this remainder prevents transfer/checkpoint frequency from destroying",
              "holder yield through repeated flooring."
            ],
            "type": "u64"
          },
          {
            "name": "interestRemainderQ64",
            "type": "u64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    }
  ]
};
