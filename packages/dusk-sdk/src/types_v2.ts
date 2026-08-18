/**
 * Program IDL in camelCase format in order to be used in JS/TS.
 *
 * Note that this is only a type helper and is not the actual IDL. The original
 * IDL can be found at `target/idl/dusk.json`.
 */
export type Dusk = {
  "address": "358bjJKXWxeAXAzteX1xTgyd9JNnjtzW8fnwCS8Da1mv",
  "metadata": {
    "name": "dusk",
    "version": "2.0.0",
    "spec": "0.1.0",
    "description": "Dusk market architecture program for Omnipair V2"
  },
  "instructions": [
    {
      "name": "addLeverageMargin",
      "discriminator": [
        56,
        245,
        65,
        29,
        221,
        125,
        238,
        241
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "positionOwner"
        },
        {
          "name": "leveragePosition",
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
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "leverage_position.position_id",
                "account": "leveragePosition"
              }
            ]
          }
        },
        {
          "name": "debtMint"
        },
        {
          "name": "debtReserveVault",
          "writable": true
        },
        {
          "name": "debtInterestVault",
          "writable": true
        },
        {
          "name": "ownerDebtAccount",
          "writable": true
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "optional": true
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
              "name": "addLeverageMarginArgs"
            }
          }
        }
      ]
    },
    {
      "name": "addLiquidity",
      "discriminator": [
        181,
        157,
        89,
        67,
        143,
        182,
        52,
        72
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "owner",
          "writable": true,
          "signer": true
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
          "name": "baseReserveVault",
          "writable": true
        },
        {
          "name": "quoteReserveVault",
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
          "name": "ownerYlpAccount",
          "writable": true
        },
        {
          "name": "baseYieldAccount",
          "docs": [
            "any liquidity mutation."
          ],
          "writable": true
        },
        {
          "name": "quoteYieldAccount",
          "docs": [
            "any liquidity mutation."
          ],
          "writable": true
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
              "name": "addLiquidityArgs"
            }
          }
        }
      ]
    },
    {
      "name": "bidLiquidationAuction",
      "discriminator": [
        6,
        223,
        30,
        228,
        147,
        197,
        27,
        49
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "liquidator",
          "writable": true,
          "signer": true
        },
        {
          "name": "debtAssetMint"
        },
        {
          "name": "collateralAssetMint"
        },
        {
          "name": "reserveVault",
          "writable": true
        },
        {
          "name": "interestVault",
          "writable": true
        },
        {
          "name": "collateralVault",
          "writable": true
        },
        {
          "name": "insuranceVault",
          "writable": true
        },
        {
          "name": "collateralInsuranceVault",
          "writable": true
        },
        {
          "name": "liquidatorDebtAccount",
          "writable": true
        },
        {
          "name": "liquidatorCollateralAccount",
          "writable": true
        },
        {
          "name": "borrowPosition",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  111,
                  114,
                  114,
                  111,
                  119,
                  95,
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "borrow_position.position_id",
                "account": "borrowPosition"
              }
            ]
          }
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "optional": true
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
              "name": "bidLiquidationAuctionArgs"
            }
          }
        }
      ]
    },
    {
      "name": "borrow",
      "discriminator": [
        228,
        253,
        131,
        202,
        207,
        116,
        89,
        18
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "debtAssetMint"
        },
        {
          "name": "collateralAssetMint"
        },
        {
          "name": "reserveVault",
          "writable": true
        },
        {
          "name": "ownerDebtAccount",
          "writable": true
        },
        {
          "name": "borrowPosition",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  111,
                  114,
                  114,
                  111,
                  119,
                  95,
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "borrow_position.position_id",
                "account": "borrowPosition"
              }
            ]
          }
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
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
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
              "name": "borrowArgs"
            }
          }
        }
      ]
    },
    {
      "name": "claimReferralInterest",
      "discriminator": [
        16,
        89,
        1,
        198,
        140,
        72,
        89,
        13
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "authority",
          "writable": true,
          "signer": true
        },
        {
          "name": "referralPartner",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  102,
                  101,
                  114,
                  114,
                  97,
                  108,
                  95,
                  112,
                  97,
                  114,
                  116,
                  110,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "authority"
              }
            ]
          }
        },
        {
          "name": "assetMint"
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  102,
                  101,
                  114,
                  114,
                  97,
                  108,
                  95,
                  97,
                  99,
                  99,
                  114,
                  117,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "referralPartner"
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "assetMint"
              }
            ]
          }
        },
        {
          "name": "interestVault",
          "writable": true
        },
        {
          "name": "recipientTokenAccount",
          "writable": true
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
      "name": "claimYield",
      "discriminator": [
        49,
        74,
        111,
        7,
        186,
        22,
        61,
        165
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
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
          "name": "assetMint"
        },
        {
          "name": "lpMint"
        },
        {
          "name": "ownerLpAccount",
          "writable": true
        },
        {
          "name": "reserveVault",
          "writable": true
        },
        {
          "name": "interestVault",
          "writable": true
        },
        {
          "name": "recipientAssetAccount",
          "writable": true
        },
        {
          "name": "yieldAccount",
          "writable": true
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
              "name": "claimYieldArgs"
            }
          }
        }
      ]
    },
    {
      "name": "closeLeverage",
      "discriminator": [
        45,
        157,
        207,
        176,
        194,
        6,
        218,
        253
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "positionOwner",
          "writable": true
        },
        {
          "name": "leveragePosition",
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
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "leverage_position.position_id",
                "account": "leveragePosition"
              }
            ]
          }
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
          "name": "debtInterestVault",
          "writable": true
        },
        {
          "name": "leverageCollateralVault",
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
                  99,
                  111,
                  108,
                  108,
                  97,
                  116,
                  101,
                  114,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "collateralMint"
              }
            ]
          }
        },
        {
          "name": "ownerDebtAccount",
          "writable": true
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "optional": true
        },
        {
          "name": "leverageDelegation",
          "optional": true
        },
        {
          "name": "delegatedProgram",
          "optional": true
        },
        {
          "name": "authority",
          "writable": true,
          "signer": true
        },
        {
          "name": "instructionsSysvar",
          "address": "Sysvar1nstructions1111111111111111111111111"
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
              "name": "closeLeverageArgs"
            }
          }
        }
      ]
    },
    {
      "name": "closeLeverageDelegation",
      "discriminator": [
        252,
        151,
        253,
        52,
        242,
        118,
        104,
        109
      ],
      "accounts": [
        {
          "name": "leverageDelegation",
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
                  100,
                  101,
                  108,
                  101,
                  103,
                  97,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "arg",
                "path": "args.position"
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
              "name": "closeLeverageDelegationArgs"
            }
          }
        }
      ]
    },
    {
      "name": "configureReferralPartner",
      "discriminator": [
        36,
        179,
        42,
        204,
        66,
        161,
        198,
        254
      ],
      "accounts": [
        {
          "name": "authoritySigner",
          "writable": true,
          "signer": true
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "referralPartner",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  102,
                  101,
                  114,
                  114,
                  97,
                  108,
                  95,
                  112,
                  97,
                  114,
                  116,
                  110,
                  101,
                  114
                ]
              },
              {
                "kind": "arg",
                "path": "args.referrer"
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
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "configureReferralPartnerArgs"
            }
          }
        }
      ]
    },
    {
      "name": "createLeverageDelegation",
      "discriminator": [
        189,
        154,
        156,
        116,
        213,
        249,
        107,
        163
      ],
      "accounts": [
        {
          "name": "market",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "leveragePosition",
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
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "leverage_position.position_id",
                "account": "leveragePosition"
              }
            ]
          }
        },
        {
          "name": "leverageDelegation",
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
                  100,
                  101,
                  108,
                  101,
                  103,
                  97,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "leveragePosition"
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
              "name": "createLeverageDelegationArgs"
            }
          }
        }
      ]
    },
    {
      "name": "createParameterProposal",
      "discriminator": [
        124,
        27,
        59,
        125,
        164,
        214,
        56,
        132
      ],
      "accounts": [
        {
          "name": "proposer",
          "writable": true,
          "signer": true
        },
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "proposal",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  97,
                  114,
                  97,
                  109,
                  101,
                  116,
                  101,
                  114,
                  95,
                  112,
                  114,
                  111,
                  112,
                  111,
                  115,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "proposer"
              },
              {
                "kind": "arg",
                "path": "nonce"
              }
            ]
          }
        },
        {
          "name": "proposalSupport",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  114,
                  111,
                  112,
                  111,
                  115,
                  97,
                  108,
                  95,
                  115,
                  117,
                  112,
                  112,
                  111,
                  114,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "proposal"
              },
              {
                "kind": "account",
                "path": "proposer"
              }
            ]
          }
        },
        {
          "name": "ylpMint",
          "writable": true
        },
        {
          "name": "proposerYlpAccount",
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
          "name": "baseHlpYlpVault",
          "docs": [
            "Token-2022 yLP vault by the handler; an existing vault is fully parsed",
            "and validated before proposal state changes."
          ],
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
                  121,
                  108,
                  112,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "market.base_side.hlp_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "ylpMint"
              }
            ]
          }
        },
        {
          "name": "quoteHlpYlpVault",
          "docs": [
            "hLP yLP vault above."
          ],
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
                  121,
                  108,
                  112,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.hlp_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "ylpMint"
              }
            ]
          }
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
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
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "createParameterProposalArgs"
            }
          }
        }
      ]
    },
    {
      "name": "decreaseLeverage",
      "discriminator": [
        177,
        163,
        187,
        72,
        82,
        174,
        68,
        229
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "positionOwner"
        },
        {
          "name": "leveragePosition",
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
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "leverage_position.position_id",
                "account": "leveragePosition"
              }
            ]
          }
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
          "name": "debtInterestVault",
          "writable": true
        },
        {
          "name": "leverageCollateralVault",
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
                  99,
                  111,
                  108,
                  108,
                  97,
                  116,
                  101,
                  114,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "collateralMint"
              }
            ]
          }
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "optional": true
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "instructionsSysvar",
          "address": "Sysvar1nstructions1111111111111111111111111"
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
              "name": "decreaseLeverageArgs"
            }
          }
        }
      ]
    },
    {
      "name": "delegatedCloseLeverage",
      "discriminator": [
        14,
        218,
        98,
        153,
        164,
        19,
        48,
        139
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "positionOwner",
          "writable": true
        },
        {
          "name": "leveragePosition",
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
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "leverage_position.position_id",
                "account": "leveragePosition"
              }
            ]
          }
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
          "name": "debtInterestVault",
          "writable": true
        },
        {
          "name": "leverageCollateralVault",
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
                  99,
                  111,
                  108,
                  108,
                  97,
                  116,
                  101,
                  114,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "collateralMint"
              }
            ]
          }
        },
        {
          "name": "ownerDebtAccount",
          "writable": true
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "optional": true
        },
        {
          "name": "leverageDelegation",
          "optional": true
        },
        {
          "name": "delegatedProgram",
          "optional": true
        },
        {
          "name": "authority",
          "writable": true,
          "signer": true
        },
        {
          "name": "instructionsSysvar",
          "address": "Sysvar1nstructions1111111111111111111111111"
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
              "name": "delegatedCloseLeverageArgs"
            }
          }
        }
      ]
    },
    {
      "name": "depositCollateral",
      "discriminator": [
        156,
        131,
        142,
        116,
        146,
        247,
        162,
        120
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
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
          "name": "assetMint"
        },
        {
          "name": "collateralVault",
          "writable": true
        },
        {
          "name": "ownerAssetAccount",
          "writable": true
        },
        {
          "name": "borrowPosition",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  111,
                  114,
                  114,
                  111,
                  119,
                  95,
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "arg",
                "path": "args.position_id"
              }
            ]
          }
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
              "name": "depositCollateralArgs"
            }
          }
        }
      ]
    },
    {
      "name": "depositSingleSided",
      "discriminator": [
        5,
        14,
        149,
        170,
        183,
        103,
        225,
        105
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
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
          "name": "ownerTargetAccount",
          "writable": true
        },
        {
          "name": "ownerHlpAccount",
          "writable": true
        },
        {
          "name": "hlpYlpAccount",
          "docs": [
            "authority are all validated before any market mutation. A System-owned",
            "empty PDA is initialized inline by the deposit handler."
          ],
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
              "name": "depositSingleSidedArgs"
            }
          }
        }
      ]
    },
    {
      "name": "donateInsurance",
      "discriminator": [
        24,
        11,
        132,
        174,
        126,
        186,
        208,
        90
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "donor",
          "writable": true,
          "signer": true
        },
        {
          "name": "assetMint"
        },
        {
          "name": "donorAssetAccount",
          "writable": true
        },
        {
          "name": "insuranceVault",
          "writable": true
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
              "name": "donateInsuranceArgs"
            }
          }
        }
      ]
    },
    {
      "name": "executeParameterProposal",
      "discriminator": [
        46,
        106,
        112,
        113,
        40,
        255,
        154,
        27
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "proposal",
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
      "args": []
    },
    {
      "name": "graduateMarket",
      "docs": [
        "Permissionless launch adapters use the same fully-backed seeding path",
        "as ordinary yLP deposits. The initializer is the one-shot authority;",
        "after this succeeds, subsequent liquidity is permissionless."
      ],
      "discriminator": [
        202,
        28,
        33,
        115,
        186,
        96,
        1,
        90
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "owner",
          "writable": true,
          "signer": true
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
          "name": "baseReserveVault",
          "writable": true
        },
        {
          "name": "quoteReserveVault",
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
          "name": "ownerYlpAccount",
          "writable": true
        },
        {
          "name": "baseYieldAccount",
          "docs": [
            "any liquidity mutation."
          ],
          "writable": true
        },
        {
          "name": "quoteYieldAccount",
          "docs": [
            "any liquidity mutation."
          ],
          "writable": true
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
              "name": "addLiquidityArgs"
            }
          }
        }
      ]
    },
    {
      "name": "increaseLeverage",
      "discriminator": [
        61,
        30,
        86,
        173,
        5,
        127,
        12,
        160
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "positionOwner"
        },
        {
          "name": "leveragePosition",
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
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "leverage_position.position_id",
                "account": "leveragePosition"
              }
            ]
          }
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
                  99,
                  111,
                  108,
                  108,
                  97,
                  116,
                  101,
                  114,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "collateralMint"
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
          "name": "instructionsSysvar",
          "address": "Sysvar1nstructions1111111111111111111111111"
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
              "name": "increaseLeverageArgs"
            }
          }
        }
      ]
    },
    {
      "name": "initFutarchyAuthority",
      "discriminator": [
        133,
        110,
        154,
        29,
        240,
        206,
        71,
        100
      ],
      "accounts": [
        {
          "name": "deployer",
          "writable": true,
          "signer": true
        },
        {
          "name": "futarchyAuthority",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "programData",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  30,
                  198,
                  73,
                  255,
                  177,
                  239,
                  53,
                  26,
                  189,
                  245,
                  158,
                  226,
                  167,
                  183,
                  246,
                  221,
                  30,
                  28,
                  81,
                  246,
                  125,
                  59,
                  35,
                  168,
                  135,
                  79,
                  228,
                  164,
                  248,
                  149,
                  245,
                  53
                ]
              }
            ],
            "program": {
              "kind": "const",
              "value": [
                2,
                168,
                246,
                145,
                78,
                136,
                161,
                176,
                226,
                16,
                21,
                62,
                247,
                99,
                174,
                43,
                0,
                194,
                185,
                61,
                22,
                193,
                36,
                210,
                192,
                83,
                122,
                16,
                4,
                128,
                0,
                0
              ]
            }
          }
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
              "name": "initFutarchyAuthorityArgs"
            }
          }
        }
      ]
    },
    {
      "name": "initializeLpMetadata",
      "discriminator": [
        214,
        99,
        201,
        159,
        220,
        88,
        74,
        27
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "lpMint",
          "writable": true
        },
        {
          "name": "lpTokenMetadata",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  101,
                  116,
                  97,
                  100,
                  97,
                  116,
                  97
                ]
              },
              {
                "kind": "const",
                "value": [
                  11,
                  112,
                  101,
                  177,
                  227,
                  209,
                  124,
                  69,
                  56,
                  157,
                  82,
                  127,
                  107,
                  4,
                  195,
                  205,
                  88,
                  184,
                  108,
                  115,
                  26,
                  160,
                  253,
                  181,
                  73,
                  182,
                  209,
                  188,
                  3,
                  248,
                  41,
                  70
                ]
              },
              {
                "kind": "account",
                "path": "lpMint"
              }
            ],
            "program": {
              "kind": "const",
              "value": [
                11,
                112,
                101,
                177,
                227,
                209,
                124,
                69,
                56,
                157,
                82,
                127,
                107,
                4,
                195,
                205,
                88,
                184,
                108,
                115,
                26,
                160,
                253,
                181,
                73,
                182,
                209,
                188,
                3,
                248,
                41,
                70
              ]
            }
          }
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        },
        {
          "name": "sysvarInstructions",
          "address": "Sysvar1nstructions1111111111111111111111111"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
        },
        {
          "name": "tokenMetadataProgram",
          "address": "metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "initializeLpMetadataArgs"
            }
          }
        }
      ]
    },
    {
      "name": "initializeLpTransferHook",
      "discriminator": [
        207,
        57,
        218,
        104,
        179,
        156,
        22,
        196
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "market"
        },
        {
          "name": "lpMint"
        },
        {
          "name": "validationAccount",
          "docs": [
            "against `lp_mint` and owned by Dusk after initialization."
          ],
          "writable": true
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "initializeMarket",
      "discriminator": [
        35,
        35,
        189,
        193,
        155,
        48,
        170,
        203
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "baseMint"
        },
        {
          "name": "quoteMint"
        },
        {
          "name": "ylpMint"
        },
        {
          "name": "baseHlpMint"
        },
        {
          "name": "quoteHlpMint"
        },
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "baseMint"
              },
              {
                "kind": "account",
                "path": "quoteMint"
              },
              {
                "kind": "arg",
                "path": "args.params_hash"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "baseReserveVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  114,
                  101,
                  115,
                  101,
                  114,
                  118,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "baseMint"
              }
            ]
          }
        },
        {
          "name": "quoteReserveVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  114,
                  101,
                  115,
                  101,
                  114,
                  118,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "quoteMint"
              }
            ]
          }
        },
        {
          "name": "baseCollateralVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  99,
                  111,
                  108,
                  108,
                  97,
                  116,
                  101,
                  114,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "baseMint"
              }
            ]
          }
        },
        {
          "name": "quoteCollateralVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  99,
                  111,
                  108,
                  108,
                  97,
                  116,
                  101,
                  114,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "quoteMint"
              }
            ]
          }
        },
        {
          "name": "baseInsuranceVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  105,
                  110,
                  115,
                  117,
                  114,
                  97,
                  110,
                  99,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "baseMint"
              }
            ]
          }
        },
        {
          "name": "quoteInsuranceVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  105,
                  110,
                  115,
                  117,
                  114,
                  97,
                  110,
                  99,
                  101
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "quoteMint"
              }
            ]
          }
        },
        {
          "name": "baseInterestVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  105,
                  110,
                  116,
                  101,
                  114,
                  101,
                  115,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "baseMint"
              }
            ]
          }
        },
        {
          "name": "quoteInterestVault",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  105,
                  110,
                  116,
                  101,
                  114,
                  101,
                  115,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "quoteMint"
              }
            ]
          }
        },
        {
          "name": "teamTreasury"
        },
        {
          "name": "teamTreasuryWsolAccount",
          "writable": true
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
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
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
              "name": "initializeMarketArgs"
            }
          }
        }
      ]
    },
    {
      "name": "initializeReferralAccrual",
      "discriminator": [
        176,
        126,
        1,
        176,
        59,
        177,
        15,
        82
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "referralPartner",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  102,
                  101,
                  114,
                  114,
                  97,
                  108,
                  95,
                  112,
                  97,
                  114,
                  116,
                  110,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "referral_partner.authority",
                "account": "referralPartner"
              }
            ]
          }
        },
        {
          "name": "market",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "assetMint"
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  102,
                  101,
                  114,
                  114,
                  97,
                  108,
                  95,
                  97,
                  99,
                  99,
                  114,
                  117,
                  97,
                  108
                ]
              },
              {
                "kind": "account",
                "path": "referralPartner"
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "assetMint"
              }
            ]
          }
        },
        {
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": []
    },
    {
      "name": "initializeYieldAccounts",
      "discriminator": [
        236,
        172,
        68,
        27,
        9,
        165,
        181,
        4
      ],
      "accounts": [
        {
          "name": "payer",
          "writable": true,
          "signer": true
        },
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "lpMint"
        },
        {
          "name": "baseMint"
        },
        {
          "name": "quoteMint"
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
          "name": "systemProgram",
          "address": "11111111111111111111111111111111"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "initializeYieldAccountsArgs"
            }
          }
        }
      ]
    },
    {
      "name": "liquidateExhaustedHlp",
      "docs": [
        "Permissionlessly closes an hLP after passive funding has exhausted its",
        "marked collateral. Insurance reimburses the borrowed-asset shortfall",
        "first; only the caller-capped remainder is socialized as unpaid funding",
        "interest. Ordinary swaps retain their existing account list."
      ],
      "discriminator": [
        240,
        209,
        152,
        23,
        129,
        28,
        166,
        8
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "caller",
          "writable": true,
          "signer": true
        },
        {
          "name": "borrowedMint"
        },
        {
          "name": "borrowedReserveVault",
          "writable": true
        },
        {
          "name": "borrowedInterestVault",
          "writable": true
        },
        {
          "name": "insuranceVault",
          "writable": true
        },
        {
          "name": "ylpMint",
          "writable": true
        },
        {
          "name": "targetHlpYlpVault",
          "writable": true
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
              "name": "liquidateExhaustedHlpArgs"
            }
          }
        }
      ]
    },
    {
      "name": "liquidateHlp",
      "docs": [
        "Permissionless critical hLP recovery. The caller supplies the hLP's",
        "borrowed asset and receives target collateral through the same exact",
        "O(1) swap/hedge transition as an ordinary recovery swap. It is live in",
        "reduce-only mode and rejects unless the selected vault is at or beyond",
        "the 9/8 funding-stress boundary."
      ],
      "discriminator": [
        21,
        45,
        129,
        212,
        235,
        140,
        120,
        17
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "trader",
          "writable": true,
          "signer": true
        },
        {
          "name": "assetInMint"
        },
        {
          "name": "assetOutMint"
        },
        {
          "name": "reserveInVault",
          "writable": true
        },
        {
          "name": "reserveOutVault",
          "writable": true
        },
        {
          "name": "traderAssetInAccount",
          "writable": true
        },
        {
          "name": "traderAssetOutAccount",
          "writable": true
        },
        {
          "name": "instructionsSysvar",
          "docs": [
            "during the configured launch rate-limit window."
          ],
          "address": "Sysvar1nstructions1111111111111111111111111"
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
              "name": "swapArgs"
            }
          }
        }
      ]
    },
    {
      "name": "liquidateLeverage",
      "discriminator": [
        188,
        132,
        10,
        83,
        171,
        78,
        116,
        41
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "positionOwner",
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
          "name": "debtInterestVault",
          "writable": true
        },
        {
          "name": "leverageCollateralVault",
          "writable": true
        },
        {
          "name": "liquidatorDebtAccount",
          "writable": true
        },
        {
          "name": "ownerDebtAccount",
          "writable": true
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "optional": true
        },
        {
          "name": "liquidator",
          "writable": true,
          "signer": true
        },
        {
          "name": "instructionsSysvar",
          "address": "Sysvar1nstructions1111111111111111111111111"
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
              "name": "liquidateLeverageArgs"
            }
          }
        }
      ]
    },
    {
      "name": "openLeverage",
      "discriminator": [
        182,
        198,
        96,
        61,
        133,
        28,
        41,
        16
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "leveragePosition",
          "docs": [
            "validated before any economic mutation."
          ],
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
          "name": "ownerDebtAccount",
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
              "name": "openLeverageArgs"
            }
          }
        }
      ]
    },
    {
      "name": "previewAddLiquidity",
      "discriminator": [
        109,
        110,
        51,
        225,
        17,
        58,
        243,
        255
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "baseMint"
        },
        {
          "name": "quoteMint"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "previewAddLiquidityArgs"
            }
          }
        }
      ],
      "returns": {
        "defined": {
          "name": "addLiquidityPreview"
        }
      }
    },
    {
      "name": "previewBorrowCapacity",
      "discriminator": [
        203,
        170,
        28,
        79,
        32,
        180,
        177,
        70
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "collateralAssetMint"
        },
        {
          "name": "debtAssetMint"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "previewBorrowCapacityArgs"
            }
          }
        }
      ],
      "returns": {
        "defined": {
          "name": "borrowCapacityPreview"
        }
      }
    },
    {
      "name": "previewBorrowPosition",
      "discriminator": [
        240,
        236,
        45,
        30,
        172,
        146,
        45,
        163
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "borrowPosition",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  111,
                  114,
                  114,
                  111,
                  119,
                  95,
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "borrow_position.position_id",
                "account": "borrowPosition"
              }
            ]
          }
        }
      ],
      "args": [],
      "returns": {
        "defined": {
          "name": "borrowPositionPreview"
        }
      }
    },
    {
      "name": "previewHlpOrderTrigger",
      "discriminator": [
        98,
        114,
        204,
        124,
        206,
        245,
        4,
        57
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "previewHlpOrderTriggerArgs"
            }
          }
        }
      ],
      "returns": {
        "defined": {
          "name": "hlpOrderTriggerPreview"
        }
      }
    },
    {
      "name": "previewMarket",
      "discriminator": [
        60,
        231,
        175,
        17,
        28,
        221,
        42,
        236
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        }
      ],
      "args": [],
      "returns": {
        "defined": {
          "name": "marketPreview"
        }
      }
    },
    {
      "name": "previewSwap",
      "discriminator": [
        98,
        74,
        197,
        115,
        135,
        154,
        188,
        70
      ],
      "accounts": [
        {
          "name": "market",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "assetInMint"
        },
        {
          "name": "assetOutMint"
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "previewSwapArgs"
            }
          }
        }
      ],
      "returns": {
        "defined": {
          "name": "swapPreview"
        }
      }
    },
    {
      "name": "queueParameterProposal",
      "discriminator": [
        194,
        89,
        164,
        68,
        253,
        251,
        21,
        108
      ],
      "accounts": [
        {
          "name": "market",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "proposal",
          "writable": true
        },
        {
          "name": "ylpMint"
        },
        {
          "name": "baseHlpYlpVault"
        },
        {
          "name": "quoteHlpYlpVault"
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
      "name": "removeLeverageMargin",
      "discriminator": [
        245,
        142,
        114,
        58,
        238,
        20,
        67,
        107
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "positionOwner"
        },
        {
          "name": "leveragePosition",
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
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "leverage_position.position_id",
                "account": "leveragePosition"
              }
            ]
          }
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
          "name": "ownerDebtAccount",
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
              "name": "removeLeverageMarginArgs"
            }
          }
        }
      ]
    },
    {
      "name": "removeLiquidity",
      "discriminator": [
        80,
        85,
        209,
        72,
        24,
        206,
        177,
        108
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
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
          "name": "baseReserveVault",
          "writable": true
        },
        {
          "name": "quoteReserveVault",
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
          "name": "ownerYlpAccount",
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
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
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
              "name": "removeLiquidityArgs"
            }
          }
        }
      ]
    },
    {
      "name": "repay",
      "discriminator": [
        234,
        103,
        67,
        82,
        208,
        234,
        219,
        166
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "debtAssetMint"
        },
        {
          "name": "reserveVault",
          "writable": true
        },
        {
          "name": "interestVault",
          "writable": true
        },
        {
          "name": "ownerDebtAccount",
          "writable": true
        },
        {
          "name": "borrowPosition",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  111,
                  114,
                  114,
                  111,
                  119,
                  95,
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "borrow_position.position_id",
                "account": "borrowPosition"
              }
            ]
          }
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "optional": true
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
              "name": "repayArgs"
            }
          }
        }
      ]
    },
    {
      "name": "setGlobalReduceOnly",
      "discriminator": [
        242,
        151,
        123,
        139,
        239,
        87,
        249,
        98
      ],
      "accounts": [
        {
          "name": "authoritySigner",
          "writable": true,
          "signer": true,
          "address": "3YL87sTCrHMB6DYKorE9CCN4dL45kZPahoREcMLDY6QV"
        },
        {
          "name": "futarchyAuthority",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "setGlobalReduceOnlyArgs"
            }
          }
        }
      ]
    },
    {
      "name": "setMarketReduceOnly",
      "discriminator": [
        178,
        78,
        219,
        198,
        188,
        185,
        181,
        88
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "authoritySigner",
          "signer": true,
          "address": "3YL87sTCrHMB6DYKorE9CCN4dL45kZPahoREcMLDY6QV"
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
              "name": "setMarketReduceOnlyArgs"
            }
          }
        }
      ]
    },
    {
      "name": "setReferralRecipient",
      "discriminator": [
        54,
        117,
        37,
        14,
        90,
        62,
        112,
        37
      ],
      "accounts": [
        {
          "name": "authority",
          "writable": true,
          "signer": true
        },
        {
          "name": "referralPartner",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  114,
                  101,
                  102,
                  101,
                  114,
                  114,
                  97,
                  108,
                  95,
                  112,
                  97,
                  114,
                  116,
                  110,
                  101,
                  114
                ]
              },
              {
                "kind": "account",
                "path": "authority"
              }
            ]
          }
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
              "name": "setReferralRecipientArgs"
            }
          }
        }
      ]
    },
    {
      "name": "setYieldRecipient",
      "discriminator": [
        178,
        211,
        80,
        10,
        138,
        52,
        188,
        22
      ],
      "accounts": [
        {
          "name": "market",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
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
          "name": "assetMint"
        },
        {
          "name": "lpMint"
        },
        {
          "name": "yieldAccount",
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
          "name": "args",
          "type": {
            "defined": {
              "name": "setYieldRecipientArgs"
            }
          }
        }
      ]
    },
    {
      "name": "settleLiquidationAuctionFloor",
      "discriminator": [
        46,
        93,
        87,
        78,
        85,
        49,
        137,
        121
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "liquidator",
          "writable": true,
          "signer": true
        },
        {
          "name": "debtAssetMint"
        },
        {
          "name": "collateralAssetMint"
        },
        {
          "name": "reserveVault",
          "writable": true
        },
        {
          "name": "interestVault",
          "writable": true
        },
        {
          "name": "collateralVault",
          "writable": true
        },
        {
          "name": "insuranceVault",
          "writable": true
        },
        {
          "name": "collateralInsuranceVault",
          "writable": true
        },
        {
          "name": "liquidatorDebtAccount",
          "writable": true
        },
        {
          "name": "liquidatorCollateralAccount",
          "writable": true
        },
        {
          "name": "borrowPosition",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  111,
                  114,
                  114,
                  111,
                  119,
                  95,
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "borrow_position.position_id",
                "account": "borrowPosition"
              }
            ]
          }
        },
        {
          "name": "referralPartner",
          "optional": true
        },
        {
          "name": "referralAccrual",
          "writable": true,
          "optional": true
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
              "name": "settleLiquidationAuctionFloorArgs"
            }
          }
        }
      ]
    },
    {
      "name": "settleProtocolAuction",
      "discriminator": [
        206,
        204,
        32,
        135,
        8,
        22,
        72,
        80
      ],
      "accounts": [
        {
          "name": "bidder",
          "writable": true,
          "signer": true
        },
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "soldMint"
        },
        {
          "name": "acceptedMint"
        },
        {
          "name": "soldVault",
          "writable": true
        },
        {
          "name": "bidderPaymentAccount",
          "writable": true
        },
        {
          "name": "bidderReceiveAccount",
          "writable": true
        },
        {
          "name": "treasuryPaymentAccount",
          "writable": true
        },
        {
          "name": "stakingVaultPaymentAccount",
          "writable": true
        },
        {
          "name": "referenceMarket"
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
              "name": "settleProtocolAuctionArgs"
            }
          }
        }
      ]
    },
    {
      "name": "supportParameterProposal",
      "discriminator": [
        151,
        82,
        42,
        209,
        219,
        155,
        3,
        9
      ],
      "accounts": [
        {
          "name": "supporter",
          "writable": true,
          "signer": true
        },
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "proposal",
          "writable": true
        },
        {
          "name": "proposalSupport",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  114,
                  111,
                  112,
                  111,
                  115,
                  97,
                  108,
                  95,
                  115,
                  117,
                  112,
                  112,
                  111,
                  114,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "proposal"
              },
              {
                "kind": "account",
                "path": "supporter"
              }
            ]
          }
        },
        {
          "name": "ylpMint",
          "writable": true
        },
        {
          "name": "supporterYlpAccount",
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
          "name": "baseHlpYlpVault"
        },
        {
          "name": "quoteHlpYlpVault"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
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
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "supportParameterProposalArgs"
            }
          }
        }
      ]
    },
    {
      "name": "swap",
      "discriminator": [
        248,
        198,
        158,
        145,
        225,
        117,
        135,
        200
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "trader",
          "writable": true,
          "signer": true
        },
        {
          "name": "assetInMint"
        },
        {
          "name": "assetOutMint"
        },
        {
          "name": "reserveInVault",
          "writable": true
        },
        {
          "name": "reserveOutVault",
          "writable": true
        },
        {
          "name": "traderAssetInAccount",
          "writable": true
        },
        {
          "name": "traderAssetOutAccount",
          "writable": true
        },
        {
          "name": "instructionsSysvar",
          "docs": [
            "during the configured launch rate-limit window."
          ],
          "address": "Sysvar1nstructions1111111111111111111111111"
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
              "name": "swapArgs"
            }
          }
        }
      ]
    },
    {
      "name": "triggerLiquidationAuction",
      "discriminator": [
        181,
        172,
        83,
        88,
        101,
        55,
        246,
        111
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "borrowPosition",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  111,
                  114,
                  114,
                  111,
                  119,
                  95,
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "borrow_position.position_id",
                "account": "borrowPosition"
              }
            ]
          }
        },
        {
          "name": "debtAssetMint"
        }
      ],
      "args": []
    },
    {
      "name": "updateFutarchyAuthority",
      "discriminator": [
        15,
        196,
        157,
        217,
        113,
        226,
        89,
        25
      ],
      "accounts": [
        {
          "name": "authoritySigner",
          "writable": true,
          "signer": true
        },
        {
          "name": "futarchyAuthority",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "updateFutarchyAuthorityArgs"
            }
          }
        }
      ]
    },
    {
      "name": "updateLeverageDelegation",
      "discriminator": [
        185,
        52,
        36,
        97,
        234,
        163,
        29,
        42
      ],
      "accounts": [
        {
          "name": "market",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "leveragePosition",
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
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "leverage_position.position_id",
                "account": "leveragePosition"
              }
            ]
          }
        },
        {
          "name": "leverageDelegation",
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
                  100,
                  101,
                  108,
                  101,
                  103,
                  97,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "leveragePosition"
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
              "name": "updateLeverageDelegationArgs"
            }
          }
        }
      ]
    },
    {
      "name": "updateProtocolAuctionConfig",
      "discriminator": [
        4,
        202,
        113,
        194,
        208,
        122,
        212,
        73
      ],
      "accounts": [
        {
          "name": "authoritySigner",
          "writable": true,
          "signer": true
        },
        {
          "name": "futarchyAuthority",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
              "name": "updateProtocolAuctionConfigArgs"
            }
          }
        }
      ]
    },
    {
      "name": "updateProtocolAuctionRecipients",
      "discriminator": [
        210,
        210,
        94,
        83,
        188,
        14,
        38,
        247
      ],
      "accounts": [
        {
          "name": "authoritySigner",
          "writable": true,
          "signer": true
        },
        {
          "name": "futarchyAuthority",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
              "name": "updateProtocolAuctionRecipientsArgs"
            }
          }
        }
      ]
    },
    {
      "name": "updateProtocolAuctionRoute",
      "discriminator": [
        37,
        160,
        194,
        28,
        53,
        78,
        43,
        119
      ],
      "accounts": [
        {
          "name": "authoritySigner",
          "signer": true
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
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
              "name": "updateProtocolAuctionRouteArgs"
            }
          }
        }
      ]
    },
    {
      "name": "updateProtocolRevenue",
      "discriminator": [
        176,
        139,
        131,
        197,
        40,
        225,
        125,
        200
      ],
      "accounts": [
        {
          "name": "authoritySigner",
          "writable": true,
          "signer": true
        },
        {
          "name": "futarchyAuthority",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
              "name": "updateProtocolRevenueArgs"
            }
          }
        }
      ]
    },
    {
      "name": "updateRevenueRecipients",
      "discriminator": [
        116,
        179,
        137,
        47,
        118,
        167,
        65,
        217
      ],
      "accounts": [
        {
          "name": "authoritySigner",
          "writable": true,
          "signer": true
        },
        {
          "name": "futarchyAuthority",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
        }
      ],
      "args": [
        {
          "name": "args",
          "type": {
            "defined": {
              "name": "updateRevenueRecipientsArgs"
            }
          }
        }
      ]
    },
    {
      "name": "withdrawCollateral",
      "discriminator": [
        115,
        135,
        168,
        106,
        139,
        214,
        138,
        150
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "futarchyAuthority",
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  102,
                  117,
                  116,
                  97,
                  114,
                  99,
                  104,
                  121,
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
          "name": "owner",
          "writable": true,
          "signer": true
        },
        {
          "name": "assetMint"
        },
        {
          "name": "collateralVault",
          "writable": true
        },
        {
          "name": "ownerAssetAccount",
          "writable": true
        },
        {
          "name": "borrowPosition",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  98,
                  111,
                  114,
                  114,
                  111,
                  119,
                  95,
                  112,
                  111,
                  115,
                  105,
                  116,
                  105,
                  111,
                  110,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "borrow_position.position_id",
                "account": "borrowPosition"
              }
            ]
          }
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
              "name": "withdrawCollateralArgs"
            }
          }
        }
      ]
    },
    {
      "name": "withdrawParameterSupport",
      "discriminator": [
        249,
        15,
        97,
        20,
        2,
        64,
        146,
        172
      ],
      "accounts": [
        {
          "name": "supporter",
          "writable": true,
          "signer": true
        },
        {
          "name": "market",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  109,
                  97,
                  114,
                  107,
                  101,
                  116,
                  95,
                  118,
                  50
                ]
              },
              {
                "kind": "account",
                "path": "market.base_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.quote_side.asset_mint",
                "account": "market"
              },
              {
                "kind": "account",
                "path": "market.params_hash",
                "account": "market"
              }
            ]
          }
        },
        {
          "name": "proposal",
          "writable": true
        },
        {
          "name": "proposalSupport",
          "writable": true,
          "pda": {
            "seeds": [
              {
                "kind": "const",
                "value": [
                  112,
                  114,
                  111,
                  112,
                  111,
                  115,
                  97,
                  108,
                  95,
                  115,
                  117,
                  112,
                  112,
                  111,
                  114,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "proposal"
              },
              {
                "kind": "account",
                "path": "supporter"
              }
            ]
          }
        },
        {
          "name": "ylpMint",
          "writable": true
        },
        {
          "name": "supporterYlpAccount",
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
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
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
      "name": "withdrawSingleSided",
      "discriminator": [
        243,
        84,
        228,
        99,
        122,
        147,
        252,
        62
      ],
      "accounts": [
        {
          "name": "market",
          "writable": true
        },
        {
          "name": "futarchyAuthority"
        },
        {
          "name": "owner",
          "writable": true,
          "signer": true
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
          "name": "ownerTargetAccount",
          "writable": true
        },
        {
          "name": "ownerHlpAccount",
          "writable": true
        },
        {
          "name": "hlpYlpAccount",
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
                  121,
                  108,
                  112,
                  95,
                  118,
                  97,
                  117,
                  108,
                  116
                ]
              },
              {
                "kind": "account",
                "path": "market"
              },
              {
                "kind": "account",
                "path": "targetHlpMint"
              },
              {
                "kind": "account",
                "path": "ylpMint"
              }
            ]
          }
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
          "name": "tokenProgram",
          "address": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
        },
        {
          "name": "token2022Program",
          "address": "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb"
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
              "name": "withdrawSingleSidedArgs"
            }
          }
        }
      ]
    }
  ],
  "accounts": [
    {
      "name": "borrowPosition",
      "discriminator": [
        243,
        140,
        20,
        139,
        32,
        243,
        114,
        55
      ]
    },
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
      "name": "parameterProposal",
      "discriminator": [
        179,
        8,
        154,
        49,
        34,
        65,
        232,
        234
      ]
    },
    {
      "name": "proposalSupport",
      "discriminator": [
        207,
        134,
        77,
        36,
        67,
        63,
        132,
        148
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
  "events": [
    {
      "name": "borrowPositionLiquidated",
      "discriminator": [
        107,
        167,
        26,
        25,
        33,
        197,
        106,
        188
      ]
    },
    {
      "name": "hlpClosed",
      "discriminator": [
        87,
        126,
        152,
        164,
        162,
        203,
        111,
        235
      ]
    },
    {
      "name": "hlpOpened",
      "discriminator": [
        188,
        231,
        244,
        52,
        5,
        151,
        236,
        84
      ]
    },
    {
      "name": "hlpTerminalLiquidated",
      "discriminator": [
        80,
        186,
        237,
        212,
        75,
        183,
        43,
        173
      ]
    },
    {
      "name": "insuranceDonated",
      "discriminator": [
        238,
        155,
        254,
        172,
        251,
        224,
        236,
        18
      ]
    },
    {
      "name": "leverageDelegationUpdated",
      "discriminator": [
        118,
        41,
        235,
        95,
        91,
        24,
        191,
        17
      ]
    },
    {
      "name": "leveragePositionClosed",
      "discriminator": [
        132,
        9,
        124,
        103,
        6,
        252,
        177,
        238
      ]
    },
    {
      "name": "leveragePositionLiquidated",
      "discriminator": [
        59,
        18,
        185,
        247,
        120,
        184,
        16,
        225
      ]
    },
    {
      "name": "leveragePositionOpened",
      "discriminator": [
        198,
        227,
        54,
        99,
        54,
        249,
        54,
        29
      ]
    },
    {
      "name": "leveragePositionUpdated",
      "discriminator": [
        195,
        171,
        245,
        43,
        32,
        163,
        44,
        170
      ]
    },
    {
      "name": "liquidityAdded",
      "discriminator": [
        154,
        26,
        221,
        108,
        238,
        64,
        217,
        161
      ]
    },
    {
      "name": "liquidityRemoved",
      "discriminator": [
        225,
        105,
        216,
        39,
        124,
        116,
        169,
        189
      ]
    },
    {
      "name": "marketCollateralDeposited",
      "discriminator": [
        41,
        53,
        157,
        172,
        249,
        159,
        63,
        60
      ]
    },
    {
      "name": "marketCollateralWithdrawn",
      "discriminator": [
        68,
        208,
        162,
        132,
        39,
        151,
        221,
        245
      ]
    },
    {
      "name": "marketCreated",
      "discriminator": [
        88,
        184,
        130,
        231,
        226,
        84,
        6,
        58
      ]
    },
    {
      "name": "marketDebtUpdated",
      "discriminator": [
        135,
        150,
        109,
        165,
        174,
        35,
        163,
        151
      ]
    },
    {
      "name": "marketHealthUpdated",
      "discriminator": [
        99,
        12,
        230,
        43,
        133,
        194,
        188,
        225
      ]
    },
    {
      "name": "marketReduceOnlyUpdated",
      "discriminator": [
        87,
        12,
        104,
        143,
        113,
        121,
        31,
        94
      ]
    },
    {
      "name": "parameterProposalCreated",
      "discriminator": [
        243,
        111,
        217,
        113,
        215,
        176,
        38,
        118
      ]
    },
    {
      "name": "parameterProposalExecuted",
      "discriminator": [
        77,
        126,
        199,
        255,
        166,
        161,
        110,
        34
      ]
    },
    {
      "name": "parameterProposalQueued",
      "discriminator": [
        166,
        188,
        191,
        35,
        42,
        165,
        21,
        49
      ]
    },
    {
      "name": "parameterProposalSupportWithdrawn",
      "discriminator": [
        204,
        113,
        93,
        73,
        179,
        229,
        233,
        123
      ]
    },
    {
      "name": "parameterProposalSupported",
      "discriminator": [
        120,
        73,
        210,
        182,
        131,
        10,
        140,
        181
      ]
    },
    {
      "name": "protocolAuctionConfigUpdated",
      "discriminator": [
        178,
        169,
        215,
        69,
        170,
        59,
        80,
        160
      ]
    },
    {
      "name": "protocolAuctionRecipientsUpdated",
      "discriminator": [
        174,
        178,
        55,
        120,
        155,
        241,
        5,
        120
      ]
    },
    {
      "name": "protocolAuctionRouteUpdated",
      "discriminator": [
        146,
        136,
        94,
        87,
        36,
        147,
        221,
        192
      ]
    },
    {
      "name": "protocolAuctionSettled",
      "discriminator": [
        11,
        230,
        199,
        245,
        28,
        133,
        107,
        3
      ]
    },
    {
      "name": "protocolAuctionSplitUpdated",
      "discriminator": [
        17,
        255,
        78,
        242,
        127,
        110,
        234,
        249
      ]
    },
    {
      "name": "referralBound",
      "discriminator": [
        12,
        28,
        152,
        148,
        55,
        210,
        102,
        190
      ]
    },
    {
      "name": "referralInterestAccrued",
      "discriminator": [
        80,
        233,
        11,
        123,
        118,
        218,
        113,
        154
      ]
    },
    {
      "name": "referralInterestClaimed",
      "discriminator": [
        103,
        79,
        19,
        199,
        132,
        222,
        156,
        190
      ]
    },
    {
      "name": "referralInterestShareCapUpdated",
      "discriminator": [
        103,
        139,
        112,
        245,
        212,
        53,
        91,
        113
      ]
    },
    {
      "name": "referralPartnerConfigured",
      "discriminator": [
        60,
        230,
        246,
        232,
        67,
        30,
        217,
        223
      ]
    },
    {
      "name": "referralRecipientUpdated",
      "discriminator": [
        51,
        4,
        221,
        175,
        134,
        8,
        235,
        222
      ]
    },
    {
      "name": "swapExecuted",
      "discriminator": [
        150,
        166,
        26,
        225,
        28,
        89,
        38,
        79
      ]
    },
    {
      "name": "yieldClaimed",
      "discriminator": [
        177,
        201,
        94,
        68,
        19,
        200,
        227,
        27
      ]
    },
    {
      "name": "yieldRecipientUpdated",
      "discriminator": [
        154,
        113,
        25,
        74,
        11,
        107,
        114,
        170
      ]
    }
  ],
  "errors": [
    {
      "code": 6000,
      "name": "invalidDeployer",
      "msg": "Invalid deployer"
    },
    {
      "code": 6001,
      "name": "argumentMissing",
      "msg": "Argument missing"
    },
    {
      "code": 6002,
      "name": "invalidSwapFeeBps",
      "msg": "Invalid swap fee bps"
    },
    {
      "code": 6003,
      "name": "invalidInterestFeeBps",
      "msg": "Invalid interest fee bps"
    },
    {
      "code": 6004,
      "name": "invalidHalfLife",
      "msg": "Invalid half life"
    },
    {
      "code": 6005,
      "name": "invalidFutarchyAuthority",
      "msg": "Invalid futarchy authority"
    },
    {
      "code": 6006,
      "name": "invalidReduceOnlyAuthority",
      "msg": "Invalid reduce-only authority"
    },
    {
      "code": 6007,
      "name": "invalidProposalMetadata",
      "msg": "Invalid parameter proposal metadata"
    },
    {
      "code": 6008,
      "name": "invalidProposalUri",
      "msg": "Invalid parameter proposal description URI"
    },
    {
      "code": 6009,
      "name": "invalidProposalDigest",
      "msg": "Parameter proposal digest does not match its immutable contents"
    },
    {
      "code": 6010,
      "name": "invalidParameterProposal",
      "msg": "Invalid parameter proposal account"
    },
    {
      "code": 6011,
      "name": "invalidProposalSupport",
      "msg": "Invalid parameter proposal support account"
    },
    {
      "code": 6012,
      "name": "proposalNotCollecting",
      "msg": "Parameter proposal is not collecting support"
    },
    {
      "code": 6013,
      "name": "proposalNotQueued",
      "msg": "Parameter proposal is not queued"
    },
    {
      "code": 6014,
      "name": "proposalSupportFrozen",
      "msg": "Queued proposal support is frozen"
    },
    {
      "code": 6015,
      "name": "proposalSponsorshipTooLow",
      "msg": "Proposal support is below the sponsorship floor"
    },
    {
      "code": 6016,
      "name": "proposalSupportInsufficient",
      "msg": "Proposal does not have a strict majority of eligible yLP"
    },
    {
      "code": 6017,
      "name": "proposalTimelockNotReady",
      "msg": "Parameter proposal timelock is not ready"
    },
    {
      "code": 6018,
      "name": "proposalExecutionWindowExpired",
      "msg": "Parameter proposal execution window has expired"
    },
    {
      "code": 6019,
      "name": "proposalStale",
      "msg": "Parameter proposal was invalidated by a same-family update"
    },
    {
      "code": 6020,
      "name": "parameterUpdateNotMeaningful",
      "msg": "Parameter update would not change the active market parameters"
    },
    {
      "code": 6021,
      "name": "invalidParameterUpdate",
      "msg": "Invalid parameter update"
    },
    {
      "code": 6022,
      "name": "utilizationGuardExceeded",
      "msg": "Parameter execution is blocked while either lending side is at or above 80% utilization"
    },
    {
      "code": 6023,
      "name": "invalidArgument",
      "msg": "Invalid argument"
    },
    {
      "code": 6024,
      "name": "amountZero",
      "msg": "Amount cannot be zero"
    },
    {
      "code": 6025,
      "name": "insufficientAmount0In",
      "msg": "Insufficient amount0 in"
    },
    {
      "code": 6026,
      "name": "insufficientAmount1In",
      "msg": "Insufficient amount1 in"
    },
    {
      "code": 6027,
      "name": "borrowingPowerExceeded",
      "msg": "Borrowing power exceeded"
    },
    {
      "code": 6028,
      "name": "invalidTokenAccount",
      "msg": "Invalid token account"
    },
    {
      "code": 6029,
      "name": "invalidTokenProgram",
      "msg": "Invalid token program"
    },
    {
      "code": 6030,
      "name": "borrowExceedsReserve",
      "msg": "Borrow exceeds reserve"
    },
    {
      "code": 6031,
      "name": "insufficientAmount0",
      "msg": "Insufficient amount0"
    },
    {
      "code": 6032,
      "name": "insufficientAmount1",
      "msg": "Insufficient amount1"
    },
    {
      "code": 6033,
      "name": "insufficientOutputAmount",
      "msg": "Insufficient output amount"
    },
    {
      "code": 6034,
      "name": "slippageExceeded",
      "msg": "Output amount below minimum requested (slippage exceeded)"
    },
    {
      "code": 6035,
      "name": "insufficientLiquidity",
      "msg": "Insufficient liquidity"
    },
    {
      "code": 6036,
      "name": "insufficientCashReserve0",
      "msg": "Insufficient cash reserve0"
    },
    {
      "code": 6037,
      "name": "insufficientCashReserve1",
      "msg": "Insufficient cash reserve1"
    },
    {
      "code": 6038,
      "name": "overflow",
      "msg": "Arithmetic overflow"
    },
    {
      "code": 6039,
      "name": "undercollateralized",
      "msg": "undercollateralized"
    },
    {
      "code": 6040,
      "name": "insufficientBalanceForCollateral",
      "msg": "Insufficient balance for collateral"
    },
    {
      "code": 6041,
      "name": "insufficientAmount",
      "msg": "Insufficient amount"
    },
    {
      "code": 6042,
      "name": "insufficientBalance",
      "msg": "User balance insufficient to cover requested amount"
    },
    {
      "code": 6043,
      "name": "insufficientDebt",
      "msg": "Insufficient debt"
    },
    {
      "code": 6044,
      "name": "userPositionNotInitialized",
      "msg": "User position not initialized"
    },
    {
      "code": 6045,
      "name": "zeroDebtAmount",
      "msg": "Zero debt amount"
    },
    {
      "code": 6046,
      "name": "notUndercollateralized",
      "msg": "Not undercollateralized"
    },
    {
      "code": 6047,
      "name": "brokenInvariant",
      "msg": "Broken invariant"
    },
    {
      "code": 6048,
      "name": "invariantOverflow",
      "msg": "Math overflow during invariant calculation"
    },
    {
      "code": 6049,
      "name": "feeMathOverflow",
      "msg": "Math overflow during fee calculation."
    },
    {
      "code": 6050,
      "name": "outputAmountOverflow",
      "msg": "Math overflow during output amount calculation."
    },
    {
      "code": 6051,
      "name": "reserveOverflow",
      "msg": "Math overflow during reserve calculation."
    },
    {
      "code": 6052,
      "name": "reserveUnderflow",
      "msg": "Math underflow during reserve calculation."
    },
    {
      "code": 6053,
      "name": "cashReserveUnderflow",
      "msg": "Math underflow during cash reserve calculation."
    },
    {
      "code": 6054,
      "name": "denominatorOverflow",
      "msg": "Math overflow during denominator calculation."
    },
    {
      "code": 6055,
      "name": "liquidityMathOverflow",
      "msg": "Math overflow during liquidity calculation"
    },
    {
      "code": 6056,
      "name": "liquiditySqrtOverflow",
      "msg": "Math overflow during liquidity square root calculation"
    },
    {
      "code": 6057,
      "name": "liquidityUnderflow",
      "msg": "Math underflow during liquidity calculation"
    },
    {
      "code": 6058,
      "name": "liquidityConversionOverflow",
      "msg": "Math overflow during liquidity conversion"
    },
    {
      "code": 6059,
      "name": "supplyOverflow",
      "msg": "Math overflow during supply calculation"
    },
    {
      "code": 6060,
      "name": "supplyUnderflow",
      "msg": "Math underflow during supply calculation"
    },
    {
      "code": 6061,
      "name": "debtMathOverflow",
      "msg": "Math overflow during debt calculation"
    },
    {
      "code": 6062,
      "name": "debtShareMathOverflow",
      "msg": "Math overflow during debt share calculation"
    },
    {
      "code": 6063,
      "name": "debtShareDivisionOverflow",
      "msg": "Math overflow during debt share division"
    },
    {
      "code": 6064,
      "name": "debtUtilizationOverflow",
      "msg": "Math overflow during debt utilization calculation"
    },
    {
      "code": 6065,
      "name": "invalidMint",
      "msg": "Invalid mint"
    },
    {
      "code": 6066,
      "name": "invalidMintLen",
      "msg": "Invalid mint length"
    },
    {
      "code": 6067,
      "name": "invalidDistribution",
      "msg": "Invalid distribution - percentages must sum to 100%"
    },
    {
      "code": 6068,
      "name": "invalidAuctionConfig",
      "msg": "Invalid protocol auction config"
    },
    {
      "code": 6069,
      "name": "staleAuctionReference",
      "msg": "Protocol auction reference price is stale"
    },
    {
      "code": 6070,
      "name": "insufficientAuctionPayment",
      "msg": "Protocol auction payment is insufficient"
    },
    {
      "code": 6071,
      "name": "invalidLpMintKey",
      "msg": "Invalid LP mint key"
    },
    {
      "code": 6072,
      "name": "invalidLpName",
      "msg": "Invalid LP name"
    },
    {
      "code": 6073,
      "name": "invalidLpSymbol",
      "msg": "Invalid LP symbol"
    },
    {
      "code": 6074,
      "name": "invalidLpUri",
      "msg": "Invalid LP URI"
    },
    {
      "code": 6075,
      "name": "accountNotEmpty",
      "msg": "Account not empty"
    },
    {
      "code": 6076,
      "name": "invalidMintAuthority",
      "msg": "Invalid mint authority"
    },
    {
      "code": 6077,
      "name": "frozenLpMint",
      "msg": "Frozen LP mint"
    },
    {
      "code": 6078,
      "name": "nonZeroSupply",
      "msg": "Non-zero supply"
    },
    {
      "code": 6079,
      "name": "wrongLpDecimals",
      "msg": "Wrong LP decimals"
    },
    {
      "code": 6080,
      "name": "unsupportedAssetDecimals",
      "msg": "Asset mint decimals exceed Dusk's 9-decimal AMM precision"
    },
    {
      "code": 6081,
      "name": "invalidVaultSameAccount",
      "msg": "Invalid vault - token_in_vault and token_out_vault must be different"
    },
    {
      "code": 6082,
      "name": "invalidVault",
      "msg": "Invalid vault"
    },
    {
      "code": 6083,
      "name": "invalidParamsHash",
      "msg": "Invalid params hash - hash does not match computed parameters"
    },
    {
      "code": 6084,
      "name": "invalidVersion",
      "msg": "Invalid version"
    },
    {
      "code": 6085,
      "name": "invalidTokenOrder",
      "msg": "Invalid token order"
    },
    {
      "code": 6086,
      "name": "invalidRateModel",
      "msg": "Invalid rate model - rate_model does not match market configuration"
    },
    {
      "code": 6087,
      "name": "invalidPositionMarket",
      "msg": "Invalid position market - position does not match market"
    },
    {
      "code": 6088,
      "name": "invalidUtilBounds",
      "msg": "Invalid utilization bounds - must satisfy: MIN <= start < end <= MAX"
    },
    {
      "code": 6089,
      "name": "invalidRateParams",
      "msg": "Invalid rate parameters - check half_life_ms, min_rate_bps, max_rate_bps, initial_rate_bps bounds"
    },
    {
      "code": 6090,
      "name": "reduceOnlyMode",
      "msg": "Operation blocked: reduce-only mode is active"
    },
    {
      "code": 6091,
      "name": "reduceOnlyHasDebt",
      "msg": "Cannot remove collateral in reduce-only mode while debt exists"
    },
    {
      "code": 6092,
      "name": "invalidInstructionsSysvar",
      "msg": "Invalid instructions sysvar"
    },
    {
      "code": 6093,
      "name": "insufficientPostWithdrawDebtCoverage",
      "msg": "Insufficient post-withdraw debt coverage"
    },
    {
      "code": 6094,
      "name": "invalidRecipient",
      "msg": "Invalid recipient - address does not match configured revenue recipient"
    },
    {
      "code": 6095,
      "name": "invalidMarket",
      "msg": "Invalid market"
    },
    {
      "code": 6096,
      "name": "invalidMarketConfig",
      "msg": "Invalid market config"
    },
    {
      "code": 6097,
      "name": "invalidSettlementPrice",
      "msg": "Invalid settlement price"
    },
    {
      "code": 6098,
      "name": "insufficientMarketShareBacking",
      "msg": "Market reserve share backing is insufficient"
    },
    {
      "code": 6099,
      "name": "invalidMarketSide",
      "msg": "Invalid market side"
    },
    {
      "code": 6100,
      "name": "invalidYieldAccount",
      "msg": "Invalid yield account"
    },
    {
      "code": 6101,
      "name": "invalidHlpVault",
      "msg": "Invalid hLP vault"
    },
    {
      "code": 6102,
      "name": "invalidHlpMintSupply",
      "msg": "Live hLP mint supply is inconsistent with stored vault supply"
    },
    {
      "code": 6103,
      "name": "notEnoughAccounts",
      "msg": "Not enough remaining accounts"
    },
    {
      "code": 6104,
      "name": "hlpSettlementUnavailable",
      "msg": "hLP settlement is unavailable"
    },
    {
      "code": 6105,
      "name": "hlpNotLiquidatable",
      "msg": "hLP funding position is not eligible for permissionless liquidation"
    },
    {
      "code": 6106,
      "name": "insufficientBorrowHeadroom",
      "msg": "Borrow headroom is insufficient"
    },
    {
      "code": 6107,
      "name": "insufficientMarketHealth",
      "msg": "Market health is insufficient"
    },
    {
      "code": 6108,
      "name": "invalidBorrowPosition",
      "msg": "Invalid borrow position"
    },
    {
      "code": 6109,
      "name": "positionNotLiquidatable",
      "msg": "Position is not liquidatable"
    },
    {
      "code": 6110,
      "name": "insufficientInsurance",
      "msg": "Insurance coverage is insufficient"
    },
    {
      "code": 6111,
      "name": "insuranceDrawExceeded",
      "msg": "Required insurance draw exceeds caller cap"
    },
    {
      "code": 6112,
      "name": "liquidationSocializationExceeded",
      "msg": "Socialized liquidation loss exceeds caller cap"
    },
    {
      "code": 6113,
      "name": "invalidClaimMint",
      "msg": "Claim mint must not charge transfer fees"
    },
    {
      "code": 6114,
      "name": "unbackedFeeLiability",
      "msg": "Fee liability is not backed by its custody balance"
    },
    {
      "code": 6115,
      "name": "invalidMarketFeeAuthority",
      "msg": "Invalid market fee authority"
    },
    {
      "code": 6116,
      "name": "invalidInitialLiquidityAuthority",
      "msg": "Invalid initial liquidity authority"
    },
    {
      "code": 6117,
      "name": "launchRateLimitSplitTransaction",
      "msg": "Launch rate limiter permits only one top-level market action per transaction"
    },
    {
      "code": 6118,
      "name": "marketReduceOnly",
      "msg": "Market is reduce-only"
    },
    {
      "code": 6119,
      "name": "marketNotStarted",
      "msg": "Market has not started"
    },
    {
      "code": 6120,
      "name": "marketMathOverflow",
      "msg": "Market math overflow"
    },
    {
      "code": 6121,
      "name": "dailyLimitExceeded",
      "msg": "Daily liquidity limit exceeded"
    },
    {
      "code": 6122,
      "name": "instructionNotLive",
      "msg": "Instruction is intentionally not live yet"
    },
    {
      "code": 6123,
      "name": "liquidationRepayTooLarge",
      "msg": "Liquidation repay amount exceeds partial liquidation cap"
    },
    {
      "code": 6124,
      "name": "leverageMultiplierTooHigh",
      "msg": "Leverage multiplier exceeds circuit breaker"
    },
    {
      "code": 6125,
      "name": "leverageInitialMarginTooLow",
      "msg": "Leverage position does not have enough initial margin"
    },
    {
      "code": 6126,
      "name": "leverageUnwindImpactTooHigh",
      "msg": "Leverage unwind impact exceeds limit"
    },
    {
      "code": 6127,
      "name": "leveragePositionNotLiquidatable",
      "msg": "Leverage position is not liquidatable"
    },
    {
      "code": 6128,
      "name": "invalidSigner",
      "msg": "Invalid signer"
    },
    {
      "code": 6129,
      "name": "invalidLeveragePosition",
      "msg": "Invalid leverage position"
    },
    {
      "code": 6130,
      "name": "invalidLeverageDelegation",
      "msg": "Invalid leverage delegation"
    },
    {
      "code": 6131,
      "name": "invalidReferralInterestShareBps",
      "msg": "Referral interest share exceeds the protocol hard cap"
    },
    {
      "code": 6132,
      "name": "invalidReferralPartner",
      "msg": "Invalid referral partner"
    },
    {
      "code": 6133,
      "name": "referralPartnerNotActive",
      "msg": "Referral partner is not active"
    },
    {
      "code": 6134,
      "name": "invalidReferralAccrual",
      "msg": "Invalid referral accrual account"
    },
    {
      "code": 6135,
      "name": "invalidLeverageCollateralMint",
      "msg": "Leverage collateral mint must not have transfer fee configuration"
    }
  ],
  "types": [
    {
      "name": "addLeverageMarginArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "amount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "addLiquidityArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "baseDepositAmount",
            "type": "u64"
          },
          {
            "name": "quoteDepositAmount",
            "type": "u64"
          },
          {
            "name": "minYlpAmount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "addLiquidityPreview",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "requestedBaseAmount",
            "type": "u64"
          },
          {
            "name": "requestedQuoteAmount",
            "type": "u64"
          },
          {
            "name": "maxBaseReserveCredit",
            "type": "u64"
          },
          {
            "name": "maxQuoteReserveCredit",
            "type": "u64"
          },
          {
            "name": "baseTransferAmount",
            "type": "u64"
          },
          {
            "name": "quoteTransferAmount",
            "type": "u64"
          },
          {
            "name": "baseTransferFee",
            "type": "u64"
          },
          {
            "name": "quoteTransferFee",
            "type": "u64"
          },
          {
            "name": "baseReserveCredit",
            "type": "u64"
          },
          {
            "name": "quoteReserveCredit",
            "type": "u64"
          },
          {
            "name": "unusedBaseAmount",
            "type": "u64"
          },
          {
            "name": "unusedQuoteAmount",
            "type": "u64"
          },
          {
            "name": "ylpAmount",
            "type": "u64"
          },
          {
            "name": "ylpSupply",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "ammConfig",
      "docs": [
        "AMM controls. A zero concentrated-liquidity share selects the full-range",
        "CPMM branch of the same explicit implementation."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "rangeWidthNad",
            "type": "u64"
          },
          {
            "name": "concentratedLiquidityShareNad",
            "type": "u64"
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
        "Embedded mutable state for the explicit curve, internal signals, and",
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
            "name": "explicitCurveCache",
            "docs": [
              "Explicit CPMM-tail/band geometry. CPMM is represented by zero",
              "concentrated liquidity in this same cache."
            ],
            "type": {
              "defined": {
                "name": "explicitCurveCache"
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
            "name": "qPerShareNad",
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
      "name": "bidLiquidationAuctionArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "repayAmount",
            "type": "u64"
          },
          {
            "name": "minCollateralOut",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "borrowArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "borrowAmount",
            "type": "u64"
          },
          {
            "name": "minDebtAmountOut",
            "type": "u64"
          },
          {
            "name": "minLiquidationCfBps",
            "type": "u16"
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
      "name": "borrowCapacityPreview",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "collateralAsset",
            "type": {
              "defined": {
                "name": "marketAsset"
              }
            }
          },
          {
            "name": "debtAsset",
            "type": {
              "defined": {
                "name": "marketAsset"
              }
            }
          },
          {
            "name": "collateralAmount",
            "type": "u64"
          },
          {
            "name": "collateralValueNad",
            "type": "u128"
          },
          {
            "name": "maxDebtByHealth",
            "type": "u64"
          },
          {
            "name": "maxDebtByCash",
            "type": "u64"
          },
          {
            "name": "maxDebtByDailyLimit",
            "type": "u64"
          },
          {
            "name": "maxDebt",
            "type": "u64"
          },
          {
            "name": "maxBorrowAmount",
            "type": "u64"
          },
          {
            "name": "borrowMarketHealthFloorBps",
            "type": "u16"
          },
          {
            "name": "globalHealthContributionCapBps",
            "type": "u16"
          },
          {
            "name": "projectedBorrowAmount",
            "type": "u64"
          },
          {
            "name": "projectedDebtAmount",
            "type": "u64"
          },
          {
            "name": "projectedHealthBps",
            "type": "u64"
          },
          {
            "name": "projectedGlobalMarketHealthBps",
            "type": "u64"
          },
          {
            "name": "projectedGlobalHealthContribution",
            "type": "u64"
          },
          {
            "name": "projectedEffectiveExistingDebtNad",
            "type": "u128"
          },
          {
            "name": "maxCfBps",
            "type": "u16"
          },
          {
            "name": "liquidationCfBps",
            "type": "u16"
          },
          {
            "name": "liquidationDebtPerCollateralPriceNad",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "borrowPosition",
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
            "name": "baseCollateral",
            "type": "u64"
          },
          {
            "name": "quoteCollateral",
            "type": "u64"
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
            "name": "baseLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "quoteLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "baseReferralPartner",
            "type": "pubkey"
          },
          {
            "name": "quoteReferralPartner",
            "type": "pubkey"
          },
          {
            "name": "baseReferralInterestShareBps",
            "type": "u16"
          },
          {
            "name": "quoteReferralInterestShareBps",
            "type": "u16"
          },
          {
            "name": "fixedBaseShares",
            "type": "u128"
          },
          {
            "name": "fixedQuoteShares",
            "type": "u128"
          },
          {
            "name": "auctionDebtAsset",
            "type": "u8"
          },
          {
            "name": "auctionStartTime",
            "type": "i64"
          },
          {
            "name": "auctionStartPriceNad",
            "type": "u64"
          },
          {
            "name": "auctionFloorPriceNad",
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
      "name": "borrowPositionLiquidated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "borrowPosition",
            "type": "pubkey"
          },
          {
            "name": "borrower",
            "type": "pubkey"
          },
          {
            "name": "liquidator",
            "type": "pubkey"
          },
          {
            "name": "debtAssetSide",
            "docs": [
              "`0` for base debt and `1` for quote debt. The collateral is the other side."
            ],
            "type": "u8"
          },
          {
            "name": "repaidAmount",
            "type": "u64"
          },
          {
            "name": "collateralSeized",
            "type": "u64"
          },
          {
            "name": "collateralToLiquidator",
            "docs": [
              "Gross collateral debited for the liquidator's transfer. Token-2022 may",
              "reduce the liquidator's net account credit."
            ],
            "type": "u64"
          },
          {
            "name": "collateralCredit",
            "docs": [
              "Net collateral credited to the liquidator after any transfer fee."
            ],
            "type": "u64"
          },
          {
            "name": "insuranceDrawn",
            "type": "u64"
          },
          {
            "name": "socializedLoss",
            "type": "u64"
          },
          {
            "name": "remainingDebt",
            "type": "u128"
          }
        ]
      }
    },
    {
      "name": "borrowPositionPreview",
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
            "name": "baseCollateral",
            "type": "u64"
          },
          {
            "name": "quoteCollateral",
            "type": "u64"
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
            "name": "baseLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "quoteLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "fixedBaseDebt",
            "type": "u128"
          },
          {
            "name": "fixedQuoteDebt",
            "type": "u128"
          },
          {
            "name": "baseDebt",
            "type": {
              "defined": {
                "name": "positionDebtSidePreview"
              }
            }
          },
          {
            "name": "quoteDebt",
            "type": {
              "defined": {
                "name": "positionDebtSidePreview"
              }
            }
          }
        ]
      }
    },
    {
      "name": "claimYieldArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "tokenKind",
            "type": {
              "defined": {
                "name": "yieldTokenKind"
              }
            }
          }
        ]
      }
    },
    {
      "name": "closeLeverageArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "minAmountOut",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "closeLeverageDelegationArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "position",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "configureReferralPartnerArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "referrer",
            "type": "pubkey"
          },
          {
            "name": "interestShareBps",
            "type": "u16"
          },
          {
            "name": "active",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "createLeverageDelegationArgs",
      "type": {
        "kind": "struct",
        "fields": [
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
          }
        ]
      }
    },
    {
      "name": "createParameterProposalArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "update",
            "type": {
              "defined": {
                "name": "marketParameterUpdate"
              }
            }
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "proposalMetadataV1"
              }
            }
          },
          {
            "name": "initialSupport",
            "type": "u64"
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
      "name": "decreaseLeverageArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "collateralAmount",
            "type": "u64"
          },
          {
            "name": "minRepayOut",
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
      "name": "delegatedCloseLeverageArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "minAmountOut",
            "type": "u64"
          },
          {
            "name": "closeBps",
            "docs": [
              "Proportion of the current position to close. `10_000` preserves the",
              "existing full-close behavior."
            ],
            "type": "u16"
          },
          {
            "name": "delegated",
            "type": {
              "defined": {
                "name": "delegatedCpiArgs"
              }
            }
          }
        ]
      }
    },
    {
      "name": "delegatedCpiArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "beforeIxData",
            "type": "bytes"
          },
          {
            "name": "afterIxData",
            "type": "bytes"
          },
          {
            "name": "beforeAccountsLen",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "depositCollateralArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "positionId",
            "type": "pubkey"
          },
          {
            "name": "depositAmount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "depositSingleSidedArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "depositAmount",
            "type": "u64"
          },
          {
            "name": "minHlpAmount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "donateInsuranceArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "asset",
            "type": "u8"
          },
          {
            "name": "amount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "explicitCurveCache",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "mathRevision",
            "type": "u8"
          },
          {
            "name": "rangeWidthNad",
            "type": "u64"
          },
          {
            "name": "concentratedLiquidityShareNad",
            "type": "u64"
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
            "name": "lowerSqrtPriceNad",
            "type": "u128"
          },
          {
            "name": "upperSqrtPriceNad",
            "type": "u128"
          }
        ]
      }
    },
    {
      "name": "feeProfile",
      "docs": [
        "Complete mutable fee surface. The fields remain embedded in their existing",
        "`MarketConfig`/`AmmConfig` locations so this view can be used by typed",
        "governance without duplicating fee state."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "baseFeeBps",
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
            "name": "divergenceFeeCoefficientNad",
            "type": "u64"
          },
          {
            "name": "volatilityFeeCoefficientNad",
            "type": "u64"
          },
          {
            "name": "volatilityHalfLifeMs",
            "type": "u64"
          },
          {
            "name": "volatilityShockCapNad",
            "type": "u64"
          },
          {
            "name": "volatilityAccumulatorCapNad",
            "type": "u64"
          },
          {
            "name": "swapFeeCollectMode",
            "type": "u8"
          },
          {
            "name": "compoundingFeeBps",
            "type": "u16"
          },
          {
            "name": "launchFeeStartBps",
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
      "name": "hlpClosed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "assetSide",
            "docs": [
              "`0` for base hLP and `1` for quote hLP."
            ],
            "type": "u8"
          },
          {
            "name": "hlpAmount",
            "type": "u64"
          },
          {
            "name": "ylpAmount",
            "type": "u64"
          },
          {
            "name": "amountOut",
            "docs": [
              "Amount credited to the owner after any output transfer fee."
            ],
            "type": "u64"
          },
          {
            "name": "debtRepaid",
            "type": "u64"
          },
          {
            "name": "interestPaid",
            "type": "u64"
          },
          {
            "name": "ylpSupply",
            "type": "u64"
          },
          {
            "name": "hlpSupply",
            "type": "u64"
          },
          {
            "name": "baseLiveReserve",
            "type": "u64"
          },
          {
            "name": "quoteLiveReserve",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "hlpOpened",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "assetSide",
            "docs": [
              "`0` for base hLP and `1` for quote hLP."
            ],
            "type": "u8"
          },
          {
            "name": "depositAmount",
            "docs": [
              "Net reserve-vault credit after any input transfer fee."
            ],
            "type": "u64"
          },
          {
            "name": "borrowedAmount",
            "type": "u64"
          },
          {
            "name": "ylpAmount",
            "type": "u64"
          },
          {
            "name": "hlpAmount",
            "type": "u64"
          },
          {
            "name": "ylpSupply",
            "type": "u64"
          },
          {
            "name": "hlpSupply",
            "type": "u64"
          },
          {
            "name": "baseLiveReserve",
            "type": "u64"
          },
          {
            "name": "quoteLiveReserve",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "hlpOrderTriggerPreview",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "principalNavPerTokenNad",
            "type": "u64"
          },
          {
            "name": "fundingAprEmaNad",
            "type": "u128"
          }
        ]
      }
    },
    {
      "name": "hlpTerminalLiquidated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "caller",
            "type": "pubkey"
          },
          {
            "name": "targetAsset",
            "type": "u8"
          },
          {
            "name": "debtClosed",
            "type": "u64"
          },
          {
            "name": "ylpBurned",
            "type": "u64"
          },
          {
            "name": "interestPaid",
            "type": "u64"
          },
          {
            "name": "insuranceDrawn",
            "type": "u64"
          },
          {
            "name": "socializedLoss",
            "type": "u64"
          },
          {
            "name": "remainingHlpSupply",
            "docs": [
              "Existing holder tokens remain burnable for zero principal while their",
              "already-checkpointed fee claims remain independently claimable."
            ],
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
      "name": "increaseLeverageArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "debtAmount",
            "type": "u64"
          },
          {
            "name": "minCollateralOut",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "initFutarchyAuthorityArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "swapBps",
            "type": "u16"
          },
          {
            "name": "interestBps",
            "type": "u16"
          },
          {
            "name": "maxReferralInterestShareBps",
            "type": "u16"
          },
          {
            "name": "futarchyTreasury",
            "type": "pubkey"
          },
          {
            "name": "futarchyTreasuryBps",
            "type": "u16"
          },
          {
            "name": "buybacksVault",
            "type": "pubkey"
          },
          {
            "name": "buybacksVaultBps",
            "type": "u16"
          },
          {
            "name": "teamTreasury",
            "type": "pubkey"
          },
          {
            "name": "teamTreasuryBps",
            "type": "u16"
          },
          {
            "name": "stakingVault",
            "type": "pubkey"
          },
          {
            "name": "feeAuctionAcceptedMint",
            "type": "pubkey"
          },
          {
            "name": "buybackAuctionAcceptedMint",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "initializeLpMetadataArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "name",
            "type": "string"
          },
          {
            "name": "symbol",
            "type": "string"
          },
          {
            "name": "uri",
            "type": "string"
          }
        ]
      }
    },
    {
      "name": "initializeMarketArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "config",
            "type": {
              "defined": {
                "name": "marketConfig"
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
            "name": "bootstrapPriceNad",
            "docs": [
              "Optional final bootstrap price imported by a permissionless launch",
              "adapter. Zero binds the reference to the first fully-backed seed."
            ],
            "type": "u64"
          },
          {
            "name": "launchFeeProgressOffset",
            "docs": [
              "Number of price-fee periods already completed before graduation."
            ],
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "initializeYieldAccountsArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "tokenKind",
            "type": {
              "defined": {
                "name": "yieldTokenKind"
              }
            }
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
      "name": "insuranceDonated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "donor",
            "type": "pubkey"
          },
          {
            "name": "asset",
            "docs": [
              "`0` for Base and `1` for Quote."
            ],
            "type": "u8"
          },
          {
            "name": "requestedAmount",
            "docs": [
              "Gross amount requested from the donor's token account."
            ],
            "type": "u64"
          },
          {
            "name": "creditedAmount",
            "docs": [
              "Net amount received after any Token-2022 transfer fee."
            ],
            "type": "u64"
          },
          {
            "name": "availableAfter",
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
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
      "name": "leverageDelegationUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "delegation",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
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
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
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
      "name": "leveragePositionClosed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "debtAssetMint",
            "type": "pubkey"
          },
          {
            "name": "collateralAssetMint",
            "type": "pubkey"
          },
          {
            "name": "debtRepaid",
            "type": "u64"
          },
          {
            "name": "interestPaid",
            "type": "u64"
          },
          {
            "name": "collateralSold",
            "type": "u64"
          },
          {
            "name": "closeoutValue",
            "type": "u64"
          },
          {
            "name": "residual",
            "type": "u64"
          },
          {
            "name": "swap",
            "type": {
              "defined": {
                "name": "leverageSwapReceipt"
              }
            }
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "leveragePositionLiquidated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "liquidator",
            "type": "pubkey"
          },
          {
            "name": "debtAssetMint",
            "type": "pubkey"
          },
          {
            "name": "collateralAssetMint",
            "type": "pubkey"
          },
          {
            "name": "debtRepaid",
            "type": "u64"
          },
          {
            "name": "interestPaid",
            "type": "u64"
          },
          {
            "name": "principalWrittenOff",
            "type": "u64"
          },
          {
            "name": "collateralSold",
            "type": "u64"
          },
          {
            "name": "closeoutValue",
            "type": "u64"
          },
          {
            "name": "liquidatorAmount",
            "type": "u64"
          },
          {
            "name": "ownerResidual",
            "type": "u64"
          },
          {
            "name": "swap",
            "type": {
              "defined": {
                "name": "leverageSwapReceipt"
              }
            }
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "leveragePositionOpened",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "debtAssetMint",
            "type": "pubkey"
          },
          {
            "name": "collateralAssetMint",
            "type": "pubkey"
          },
          {
            "name": "marginAmount",
            "type": "u64"
          },
          {
            "name": "borrowedAmount",
            "type": "u64"
          },
          {
            "name": "debtAmount",
            "type": "u64"
          },
          {
            "name": "debtShares",
            "type": "u128"
          },
          {
            "name": "collateralAmount",
            "type": "u64"
          },
          {
            "name": "closeoutValue",
            "type": "u64"
          },
          {
            "name": "equity",
            "type": "u64"
          },
          {
            "name": "multiplierBps",
            "type": "u64"
          },
          {
            "name": "swap",
            "type": {
              "defined": {
                "name": "leverageSwapReceipt"
              }
            }
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "leveragePositionUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "debtAssetMint",
            "type": "pubkey"
          },
          {
            "name": "collateralAssetMint",
            "type": "pubkey"
          },
          {
            "name": "borrowedAmount",
            "type": "u64"
          },
          {
            "name": "debtDelta",
            "type": "i64"
          },
          {
            "name": "collateralDelta",
            "type": "i64"
          },
          {
            "name": "debtAmount",
            "type": "u64"
          },
          {
            "name": "debtShares",
            "type": "u128"
          },
          {
            "name": "collateralAmount",
            "type": "u64"
          },
          {
            "name": "closeoutValue",
            "type": "u64"
          },
          {
            "name": "ownerCredit",
            "docs": [
              "Net tokens paid to the owner by this update, if any."
            ],
            "type": "u64"
          },
          {
            "name": "swap",
            "type": {
              "option": {
                "defined": {
                  "name": "leverageSwapReceipt"
                }
              }
            }
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "leverageSwapReceipt",
      "docs": [
        "Actual AMM receipt embedded in a leverage action.",
        "`None` on `LeveragePositionUpdated` means the action was margin-only."
      ],
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "assetInSide",
            "type": "u8"
          },
          {
            "name": "feeAssetSide",
            "type": "u8"
          },
          {
            "name": "amountIn",
            "type": "u64"
          },
          {
            "name": "amountOut",
            "type": "u64"
          },
          {
            "name": "grossAmountOut",
            "type": "u64"
          },
          {
            "name": "amountInAfterFee",
            "type": "u64"
          },
          {
            "name": "baseFee",
            "type": "u64"
          },
          {
            "name": "divergenceFee",
            "type": "u64"
          },
          {
            "name": "volatilityFee",
            "type": "u64"
          },
          {
            "name": "retainedFee",
            "type": "u64"
          },
          {
            "name": "compoundedFee",
            "type": "u64"
          },
          {
            "name": "claimableFeeCredit",
            "docs": [
              "Actual reserve-vault credit after any Token-2022 transfer fee."
            ],
            "type": "u64"
          },
          {
            "name": "baseLiveReserve",
            "docs": [
              "Final executable reserves after retention and inline hLP correction."
            ],
            "type": "u64"
          },
          {
            "name": "quoteLiveReserve",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "liquidateExhaustedHlpArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "targetAsset",
            "type": "u8"
          },
          {
            "name": "maxInsuranceDraw",
            "type": "u64"
          },
          {
            "name": "maxSocializedLoss",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "liquidateLeverageArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "debtAsset",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "liquidityAdded",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "baseReserveCredit",
            "type": "u64"
          },
          {
            "name": "quoteReserveCredit",
            "type": "u64"
          },
          {
            "name": "ylpAmount",
            "type": "u64"
          },
          {
            "name": "ylpSupply",
            "type": "u64"
          },
          {
            "name": "baseLiveReserve",
            "type": "u64"
          },
          {
            "name": "quoteLiveReserve",
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "liquidityRemoved",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "ylpAmount",
            "type": "u64"
          },
          {
            "name": "baseReserveDebit",
            "docs": [
              "Gross amounts debited from the reserve vaults."
            ],
            "type": "u64"
          },
          {
            "name": "quoteReserveDebit",
            "type": "u64"
          },
          {
            "name": "baseOwnerCredit",
            "docs": [
              "Net amounts credited to the owner after any Token-2022 transfer fee."
            ],
            "type": "u64"
          },
          {
            "name": "quoteOwnerCredit",
            "type": "u64"
          },
          {
            "name": "ylpSupply",
            "type": "u64"
          },
          {
            "name": "baseLiveReserve",
            "type": "u64"
          },
          {
            "name": "quoteLiveReserve",
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
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
      "name": "marketAsset",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "base"
          },
          {
            "name": "quote"
          }
        ]
      }
    },
    {
      "name": "marketCollateralDeposited",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "assetMint",
            "type": "pubkey"
          },
          {
            "name": "collateralCredit",
            "type": "u64"
          },
          {
            "name": "baseCollateral",
            "type": "u64"
          },
          {
            "name": "quoteCollateral",
            "type": "u64"
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
            "name": "baseLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "quoteLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "marketCollateralWithdrawn",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "assetMint",
            "type": "pubkey"
          },
          {
            "name": "collateralDebit",
            "type": "u64"
          },
          {
            "name": "assetCredit",
            "type": "u64"
          },
          {
            "name": "baseCollateral",
            "type": "u64"
          },
          {
            "name": "quoteCollateral",
            "type": "u64"
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
            "name": "baseLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "quoteLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
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
            "name": "qEmaHalfLifeMs",
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
      "name": "marketCreated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "baseMint",
            "type": "pubkey"
          },
          {
            "name": "quoteMint",
            "type": "pubkey"
          },
          {
            "name": "ylpMint",
            "type": "pubkey"
          },
          {
            "name": "baseCollateralVault",
            "type": "pubkey"
          },
          {
            "name": "quoteCollateralVault",
            "type": "pubkey"
          },
          {
            "name": "baseInsuranceVault",
            "type": "pubkey"
          },
          {
            "name": "quoteInsuranceVault",
            "type": "pubkey"
          },
          {
            "name": "baseHlpMint",
            "type": "pubkey"
          },
          {
            "name": "quoteHlpMint",
            "type": "pubkey"
          },
          {
            "name": "targetHlpLeverageBps",
            "type": "u16"
          },
          {
            "name": "swapFeeBps",
            "type": "u16"
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
            "type": "pubkey"
          },
          {
            "name": "launchReferencePriceNad",
            "type": "u64"
          },
          {
            "name": "launchFeeProgressOffset",
            "type": "u16"
          },
          {
            "name": "version",
            "type": "u8"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "marketDebtUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "debtAssetMint",
            "type": "pubkey"
          },
          {
            "name": "debtDelta",
            "type": "i64"
          },
          {
            "name": "cashDebit",
            "docs": [
              "Gross source-account debit and net destination-account credit."
            ],
            "type": "u64"
          },
          {
            "name": "cashCredit",
            "type": "u64"
          },
          {
            "name": "interestPaid",
            "type": "u64"
          },
          {
            "name": "fixedBaseDebt",
            "type": "u128"
          },
          {
            "name": "fixedQuoteDebt",
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
            "name": "baseLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "quoteLiquidationCfBps",
            "type": "u16"
          },
          {
            "name": "baseDebtHealthBps",
            "type": "u64"
          },
          {
            "name": "quoteDebtHealthBps",
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "marketEventMetadata",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "signer",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "slot",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "marketHealth",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "globalHealthBaseContributionForQuoteDebt",
            "type": "u64"
          },
          {
            "name": "globalHealthQuoteContributionForBaseDebt",
            "type": "u64"
          },
          {
            "name": "effectiveBaseDebtNad",
            "type": "u128"
          },
          {
            "name": "effectiveQuoteDebtNad",
            "type": "u128"
          },
          {
            "name": "baseDebtHealthBps",
            "type": "u64"
          },
          {
            "name": "quoteDebtHealthBps",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "marketHealthUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
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
            "name": "effectiveBaseDebtNad",
            "type": "u128"
          },
          {
            "name": "effectiveQuoteDebtNad",
            "type": "u128"
          },
          {
            "name": "baseDebtHealthBps",
            "type": "u64"
          },
          {
            "name": "quoteDebtHealthBps",
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "marketParameterUpdate",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "fee",
            "fields": [
              {
                "defined": {
                  "name": "feeProfile"
                }
              }
            ]
          },
          {
            "name": "concentration",
            "fields": [
              {
                "name": "rangeWidthNad",
                "type": "u64"
              },
              {
                "name": "concentratedLiquidityShareNad",
                "type": "u64"
              }
            ]
          },
          {
            "name": "irm",
            "fields": [
              {
                "defined": {
                  "name": "irmConfig"
                }
              }
            ]
          },
          {
            "name": "emaHalfLives",
            "fields": [
              {
                "name": "priceMs",
                "type": "u64"
              },
              {
                "name": "directionalPriceMs",
                "type": "u64"
              },
              {
                "name": "qMs",
                "type": "u64"
              },
              {
                "name": "centerPriceMs",
                "type": "u64"
              }
            ]
          },
          {
            "name": "dailyBorrowLimit",
            "fields": [
              {
                "name": "maxDailyBorrowBps",
                "type": "u16"
              }
            ]
          },
          {
            "name": "centerController",
            "fields": [
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
              }
            ]
          },
          {
            "name": "insuranceDrawCaps",
            "fields": [
              {
                "name": "perEventBps",
                "type": "u16"
              },
              {
                "name": "perDayBps",
                "type": "u16"
              }
            ]
          }
        ]
      }
    },
    {
      "name": "marketPreview",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "slot",
            "type": "u64"
          },
          {
            "name": "base",
            "type": {
              "defined": {
                "name": "previewSide"
              }
            }
          },
          {
            "name": "quote",
            "type": {
              "defined": {
                "name": "previewSide"
              }
            }
          },
          {
            "name": "liquidityNad",
            "type": "u128"
          },
          {
            "name": "health",
            "type": {
              "defined": {
                "name": "marketHealth"
              }
            }
          },
          {
            "name": "amm",
            "type": {
              "defined": {
                "name": "previewAmm"
              }
            }
          }
        ]
      }
    },
    {
      "name": "marketReduceOnlyUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "reduceOnly",
            "type": "bool"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
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
      "name": "openLeverageArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "positionId",
            "type": "pubkey"
          },
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "marginAmount",
            "type": "u64"
          },
          {
            "name": "multiplierBps",
            "type": "u64"
          },
          {
            "name": "minCollateralOut",
            "type": "u64"
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
      "name": "parameterFamily",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "fee"
          },
          {
            "name": "concentration"
          },
          {
            "name": "irm"
          },
          {
            "name": "emaHalfLives"
          },
          {
            "name": "dailyBorrowLimit"
          },
          {
            "name": "centerController"
          },
          {
            "name": "insurance"
          }
        ]
      }
    },
    {
      "name": "parameterProposal",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "proposer",
            "type": "pubkey"
          },
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "family",
            "type": {
              "defined": {
                "name": "parameterFamily"
              }
            }
          },
          {
            "name": "familyRevision",
            "type": "u64"
          },
          {
            "name": "update",
            "type": {
              "defined": {
                "name": "marketParameterUpdate"
              }
            }
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "proposalMetadataV1"
              }
            }
          },
          {
            "name": "digest",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "status",
            "type": {
              "defined": {
                "name": "parameterProposalStatus"
              }
            }
          },
          {
            "name": "sponsorshipFloor",
            "docs": [
              "Frozen minimum support needed to keep this proposal alive after its",
              "initial 1% sponsorship burn-lock."
            ],
            "type": "u64"
          },
          {
            "name": "totalLocked",
            "type": "u64"
          },
          {
            "name": "queuedSupport",
            "docs": [
              "Immutable queue-time numerator and direct-yLP denominator."
            ],
            "type": "u64"
          },
          {
            "name": "queuedEligibleYlp",
            "type": "u64"
          },
          {
            "name": "createdAt",
            "type": "i64"
          },
          {
            "name": "queuedAt",
            "type": "i64"
          },
          {
            "name": "executeAfter",
            "type": "i64"
          },
          {
            "name": "executionDeadline",
            "type": "i64"
          },
          {
            "name": "bump",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "parameterProposalCreated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "proposal",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "proposer",
            "type": "pubkey"
          },
          {
            "name": "nonce",
            "type": "u64"
          },
          {
            "name": "family",
            "type": "u8"
          },
          {
            "name": "familyRevision",
            "type": "u64"
          },
          {
            "name": "digest",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "sponsorshipFloor",
            "type": "u64"
          },
          {
            "name": "initialSupport",
            "type": "u64"
          },
          {
            "name": "status",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "parameterProposalExecuted",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "proposal",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "family",
            "type": "u8"
          },
          {
            "name": "newFamilyRevision",
            "type": "u64"
          },
          {
            "name": "executedAt",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "parameterProposalQueued",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "proposal",
            "type": "pubkey"
          },
          {
            "name": "totalLocked",
            "type": "u64"
          },
          {
            "name": "eligibleSupply",
            "type": "u64"
          },
          {
            "name": "queuedAt",
            "type": "i64"
          },
          {
            "name": "executeAfter",
            "type": "i64"
          },
          {
            "name": "executionDeadline",
            "type": "i64"
          }
        ]
      }
    },
    {
      "name": "parameterProposalStatus",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "collecting"
          },
          {
            "name": "queued"
          },
          {
            "name": "executed"
          },
          {
            "name": "cancelled"
          },
          {
            "name": "expired"
          },
          {
            "name": "stale"
          }
        ]
      }
    },
    {
      "name": "parameterProposalSupportWithdrawn",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "proposal",
            "type": "pubkey"
          },
          {
            "name": "supporter",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "totalLocked",
            "type": "u64"
          },
          {
            "name": "status",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "parameterProposalSupported",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "proposal",
            "type": "pubkey"
          },
          {
            "name": "supporter",
            "type": "pubkey"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "supporterLocked",
            "type": "u64"
          },
          {
            "name": "totalLocked",
            "type": "u64"
          },
          {
            "name": "status",
            "type": "u8"
          }
        ]
      }
    },
    {
      "name": "positionDebtSidePreview",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "debtAsset",
            "type": {
              "defined": {
                "name": "marketAsset"
              }
            }
          },
          {
            "name": "collateralAsset",
            "type": {
              "defined": {
                "name": "marketAsset"
              }
            }
          },
          {
            "name": "fixedDebt",
            "type": "u128"
          },
          {
            "name": "collateralAmount",
            "type": "u64"
          },
          {
            "name": "globalHealthContribution",
            "type": "u64"
          },
          {
            "name": "collateralValueNad",
            "type": "u128"
          },
          {
            "name": "healthBps",
            "type": "u64"
          },
          {
            "name": "maxCfBps",
            "type": "u16"
          },
          {
            "name": "liquidationCfBps",
            "type": "u16"
          },
          {
            "name": "liquidationReferencePriceNad",
            "type": "u64"
          },
          {
            "name": "liquidationHealthBps",
            "type": "u64"
          },
          {
            "name": "isLiquidatable",
            "type": "bool"
          },
          {
            "name": "liquidationIncentiveBps",
            "type": "u16"
          },
          {
            "name": "insuranceFundingBps",
            "type": "u16"
          },
          {
            "name": "totalPenaltyBps",
            "type": "u16"
          },
          {
            "name": "maxRepayAmount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "previewAddLiquidityArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "baseDepositAmount",
            "type": "u64"
          },
          {
            "name": "quoteDepositAmount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "previewAmm",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "initialized",
            "type": "bool"
          },
          {
            "name": "executableBaseReserve",
            "type": "u64"
          },
          {
            "name": "executableQuoteReserve",
            "type": "u64"
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
            "name": "volatilityAccumulatorNad",
            "type": "u64"
          },
          {
            "name": "decayedVolatilityNad",
            "type": "u64"
          },
          {
            "name": "balancedEquivalentQNad",
            "type": "u128"
          },
          {
            "name": "qPerShareNad",
            "type": "u128"
          },
          {
            "name": "protectedFloorPerShareNad",
            "type": "u128"
          },
          {
            "name": "protectedProfitPerShareNad",
            "type": "u128"
          },
          {
            "name": "retentionRequiredNad",
            "type": "u128"
          },
          {
            "name": "retentionStopNad",
            "type": "u128"
          },
          {
            "name": "retentionHardCapNad",
            "type": "u128"
          },
          {
            "name": "retentionActive",
            "type": "bool"
          },
          {
            "name": "retentionTargetSaturated",
            "type": "bool"
          },
          {
            "name": "retentionTargetStale",
            "type": "bool"
          },
          {
            "name": "protectedRecenterBaseReserve",
            "type": "u64"
          },
          {
            "name": "protectedRecenterQuoteReserve",
            "type": "u64"
          },
          {
            "name": "rangeWidthNad",
            "type": "u64"
          },
          {
            "name": "concentratedLiquidityShareNad",
            "type": "u64"
          },
          {
            "name": "lowerRangePriceNad",
            "type": "u64"
          },
          {
            "name": "upperRangePriceNad",
            "type": "u64"
          },
          {
            "name": "explicitCurveBranch",
            "type": "u8"
          },
          {
            "name": "ordinaryBaseReserveNad",
            "type": "u128"
          },
          {
            "name": "ordinaryQuoteReserveNad",
            "type": "u128"
          }
        ]
      }
    },
    {
      "name": "previewBorrowCapacityArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "collateralAmount",
            "type": "u64"
          },
          {
            "name": "projectedBorrowAmount",
            "type": {
              "option": "u64"
            }
          }
        ]
      }
    },
    {
      "name": "previewHlpOrderTriggerArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "targetAsset",
            "type": "u8"
          },
          {
            "name": "hlpAmount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "previewSide",
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
            "type": "u64"
          },
          {
            "name": "quoteHlpBackingInventory",
            "type": "u64"
          },
          {
            "name": "ylpSupply",
            "type": "u64"
          },
          {
            "name": "ylpExchangeRateNad",
            "type": "u128"
          },
          {
            "name": "spotPriceNad",
            "type": "u64"
          },
          {
            "name": "priceEmaNad",
            "type": "u64"
          },
          {
            "name": "directionalPriceEmaNad",
            "type": "u64"
          },
          {
            "name": "conservativeDepthNad",
            "type": "u128"
          },
          {
            "name": "borrowIndexNad",
            "type": "u128"
          },
          {
            "name": "rateAtTargetNad",
            "type": "u128"
          },
          {
            "name": "borrowAprNad",
            "type": "u128"
          },
          {
            "name": "utilizationBps",
            "type": "u64"
          },
          {
            "name": "fixedDebt",
            "type": "u128"
          },
          {
            "name": "isolatedDebt",
            "type": "u128"
          },
          {
            "name": "hlpFundingDebt",
            "type": "u128"
          },
          {
            "name": "totalDebt",
            "type": "u128"
          },
          {
            "name": "dailyBorrowLimit",
            "type": "u64"
          },
          {
            "name": "dailyBorrowRemaining",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "previewSwapArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "exactAssetIn",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "proposalMetadataV1",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "version",
            "type": "u8"
          },
          {
            "name": "title",
            "type": "string"
          },
          {
            "name": "descriptionUri",
            "type": "string"
          },
          {
            "name": "descriptionSha256",
            "type": {
              "array": [
                "u8",
                32
              ]
            }
          },
          {
            "name": "descriptionLen",
            "type": "u32"
          }
        ]
      }
    },
    {
      "name": "proposalSupport",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "proposal",
            "type": "pubkey"
          },
          {
            "name": "supporter",
            "type": "pubkey"
          },
          {
            "name": "lockedAmount",
            "type": "u64"
          },
          {
            "name": "baseYield",
            "type": {
              "defined": {
                "name": "virtualYieldLedger"
              }
            }
          },
          {
            "name": "quoteYield",
            "type": {
              "defined": {
                "name": "virtualYieldLedger"
              }
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
      "name": "protocolAuctionConfigUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "lane",
            "type": "u8"
          },
          {
            "name": "acceptedMint",
            "type": "pubkey"
          },
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
          },
          {
            "name": "signer",
            "type": "pubkey"
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
      "name": "protocolAuctionLane",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "fee"
          },
          {
            "name": "buyback"
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
      "name": "protocolAuctionRecipientsUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "lane",
            "type": "u8"
          },
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
          },
          {
            "name": "signer",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "protocolAuctionRouteUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "lane",
            "type": "u8"
          },
          {
            "name": "side",
            "type": "u8"
          },
          {
            "name": "soldMint",
            "type": "pubkey"
          },
          {
            "name": "acceptedMint",
            "type": "pubkey"
          },
          {
            "name": "referenceMarket",
            "docs": [
              "`Pubkey::default()` restores the direct-market-only policy."
            ],
            "type": "pubkey"
          },
          {
            "name": "signer",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "protocolAuctionSettled",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "referenceMarket",
            "type": "pubkey"
          },
          {
            "name": "lane",
            "type": "u8"
          },
          {
            "name": "source",
            "type": "u8"
          },
          {
            "name": "side",
            "type": "u8"
          },
          {
            "name": "bidder",
            "type": "pubkey"
          },
          {
            "name": "soldMint",
            "type": "pubkey"
          },
          {
            "name": "acceptedMint",
            "type": "pubkey"
          },
          {
            "name": "soldAmount",
            "type": "u64"
          },
          {
            "name": "paymentAmount",
            "type": "u64"
          },
          {
            "name": "treasuryAmount",
            "type": "u64"
          },
          {
            "name": "stakingVaultAmount",
            "type": "u64"
          },
          {
            "name": "referencePriceNad",
            "type": "u64"
          },
          {
            "name": "auctionPriceNad",
            "type": "u64"
          },
          {
            "name": "remainingFeeLiability",
            "type": "u64"
          },
          {
            "name": "remainingBuybackLiability",
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
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
      "name": "protocolAuctionSplitUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "feeAuctionBps",
            "type": "u16"
          },
          {
            "name": "buybackAuctionBps",
            "type": "u16"
          },
          {
            "name": "signer",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "protocolRevenueSource",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "swap"
          },
          {
            "name": "interest"
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
      "name": "referralBound",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "referrer",
            "type": "pubkey"
          },
          {
            "name": "referralPartner",
            "type": "pubkey"
          },
          {
            "name": "assetMint",
            "type": "pubkey"
          },
          {
            "name": "interestShareBps",
            "type": "u16"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "referralInterestAccrued",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "position",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "referrer",
            "type": "pubkey"
          },
          {
            "name": "referralPartner",
            "type": "pubkey"
          },
          {
            "name": "referralAccrual",
            "type": "pubkey"
          },
          {
            "name": "assetMint",
            "type": "pubkey"
          },
          {
            "name": "interestPaid",
            "type": "u64"
          },
          {
            "name": "interestVaultCredit",
            "type": "u64"
          },
          {
            "name": "protocolInterestRevenue",
            "type": "u64"
          },
          {
            "name": "interestShareBps",
            "type": "u16"
          },
          {
            "name": "accruedAmount",
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "referralInterestClaimed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "referralPartner",
            "type": "pubkey"
          },
          {
            "name": "referralAccrual",
            "type": "pubkey"
          },
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "recipient",
            "type": "pubkey"
          },
          {
            "name": "assetMint",
            "type": "pubkey"
          },
          {
            "name": "vaultDebit",
            "type": "u64"
          },
          {
            "name": "recipientCredit",
            "type": "u64"
          },
          {
            "name": "remainingAccrual",
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "referralInterestShareCapUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "maxReferralInterestShareBps",
            "type": "u16"
          },
          {
            "name": "signer",
            "type": "pubkey"
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
      "name": "referralPartnerConfigured",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "referralPartner",
            "type": "pubkey"
          },
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
            "name": "signer",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "referralRecipientUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "referralPartner",
            "type": "pubkey"
          },
          {
            "name": "authority",
            "type": "pubkey"
          },
          {
            "name": "recipient",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "removeLeverageMarginArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "debtAsset",
            "type": "u8"
          },
          {
            "name": "amount",
            "type": "u64"
          },
          {
            "name": "minAmountOut",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "removeLiquidityArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "ylpAmount",
            "type": "u64"
          },
          {
            "name": "minBaseAmountOut",
            "type": "u64"
          },
          {
            "name": "minQuoteAmountOut",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "repayArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "repayAmount",
            "type": "u64"
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
            "name": "cachedQNad",
            "docs": [
              "Last observed balanced-equivalent CONCENTRATED depth."
            ],
            "type": "u128"
          },
          {
            "name": "qEmaNad",
            "docs": [
              "EMA of balanced-equivalent CONCENTRATED depth. This replaces the CPMM `K` EMA",
              "while retaining the same serialized width."
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
      "name": "setGlobalReduceOnlyArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "reduceOnly",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "setMarketReduceOnlyArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "reduceOnly",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "setReferralRecipientArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "recipient",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "setYieldRecipientArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "tokenKind",
            "type": {
              "defined": {
                "name": "yieldTokenKind"
              }
            }
          },
          {
            "name": "recipient",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "settleLiquidationAuctionFloorArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "repayAmount",
            "type": "u64"
          },
          {
            "name": "minCollateralOut",
            "type": "u64"
          },
          {
            "name": "maxInsuranceDraw",
            "type": "u64"
          },
          {
            "name": "maxSocializedLoss",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "settleProtocolAuctionArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "lane",
            "type": {
              "defined": {
                "name": "protocolAuctionLane"
              }
            }
          },
          {
            "name": "source",
            "type": {
              "defined": {
                "name": "protocolRevenueSource"
              }
            }
          },
          {
            "name": "soldAmount",
            "type": "u64"
          },
          {
            "name": "maxPaymentAmount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "supportParameterProposalArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "amount",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "swapArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "exactAssetIn",
            "type": "u64"
          },
          {
            "name": "minAssetOut",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "swapExecuted",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "trader",
            "type": "pubkey"
          },
          {
            "name": "assetInSide",
            "docs": [
              "`0` for base input and `1` for quote input."
            ],
            "type": "u8"
          },
          {
            "name": "amountIn",
            "docs": [
              "Exact amount debited from the trader's input account."
            ],
            "type": "u64"
          },
          {
            "name": "amountOut",
            "docs": [
              "Amount credited to the trader after any output transfer fee."
            ],
            "type": "u64"
          },
          {
            "name": "grossAmountOut",
            "docs": [
              "Curve output before an output-denominated fee and transfer fee."
            ],
            "type": "u64"
          },
          {
            "name": "feeAssetSide",
            "docs": [
              "`0` for Base and `1` for Quote."
            ],
            "type": "u8"
          },
          {
            "name": "amountInAfterFee",
            "docs": [
              "Input applied to the invariant after all swap fees."
            ],
            "type": "u64"
          },
          {
            "name": "baseFee",
            "type": "u64"
          },
          {
            "name": "divergenceFee",
            "type": "u64"
          },
          {
            "name": "volatilityFee",
            "type": "u64"
          },
          {
            "name": "retainedFee",
            "docs": [
              "Dynamic surcharge retained as executable principal."
            ],
            "type": "u64"
          },
          {
            "name": "compoundedFee",
            "docs": [
              "LP-owned fee compounded into reserve principal."
            ],
            "type": "u64"
          },
          {
            "name": "hlpRecoveryTargetAsset",
            "docs": [
              "Extra output funded by deleveraging the stressed hLP."
            ],
            "type": "u8"
          },
          {
            "name": "hlpRecoveryFundingGap",
            "type": "u64"
          },
          {
            "name": "hlpRecoveryMatchedInput",
            "type": "u64"
          },
          {
            "name": "hlpRecoveryBonusOutput",
            "type": "u64"
          },
          {
            "name": "hlpRecoveryDiscountBps",
            "type": "u16"
          },
          {
            "name": "hlpRecoveryCritical",
            "type": "bool"
          },
          {
            "name": "baseLiveReserve",
            "docs": [
              "Final executable reserves after retention and inline hLP correction."
            ],
            "type": "u64"
          },
          {
            "name": "quoteLiveReserve",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "swapPreview",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "assetIn",
            "type": {
              "defined": {
                "name": "marketAsset"
              }
            }
          },
          {
            "name": "assetOut",
            "type": {
              "defined": {
                "name": "marketAsset"
              }
            }
          },
          {
            "name": "exactAssetIn",
            "type": "u64"
          },
          {
            "name": "transferFee",
            "type": "u64"
          },
          {
            "name": "reserveCredit",
            "docs": [
              "Actual credit received by the reserve vault from the user transfer."
            ],
            "type": "u64"
          },
          {
            "name": "feeAsset",
            "type": {
              "defined": {
                "name": "marketAsset"
              }
            }
          },
          {
            "name": "amountOut",
            "type": "u64"
          },
          {
            "name": "grossAmountOut",
            "type": "u64"
          },
          {
            "name": "reserveInLiveReserve",
            "type": "u64"
          },
          {
            "name": "reserveOutLiveReserve",
            "type": "u64"
          },
          {
            "name": "baseFeeDebit",
            "type": "u64"
          },
          {
            "name": "divergenceSurchargeDebit",
            "type": "u64"
          },
          {
            "name": "volatilitySurchargeDebit",
            "type": "u64"
          },
          {
            "name": "dynamicSurchargeDebit",
            "type": "u64"
          },
          {
            "name": "totalFeeDebit",
            "type": "u64"
          },
          {
            "name": "retainedSurcharge",
            "type": "u64"
          },
          {
            "name": "distributedSurchargeDebit",
            "type": "u64"
          },
          {
            "name": "claimableFeeDebit",
            "type": "u64"
          },
          {
            "name": "compoundedFeeDebit",
            "docs": [
              "LP-owned fee converted into reserve principal at the configured rate."
            ],
            "type": "u64"
          },
          {
            "name": "compoundingFeeBps",
            "type": "u16"
          },
          {
            "name": "amountInForQuote",
            "type": "u64"
          },
          {
            "name": "reserveInputCredit",
            "type": "u64"
          },
          {
            "name": "baseFeeCredit",
            "type": "u64"
          },
          {
            "name": "distributedSurchargeCredit",
            "type": "u64"
          },
          {
            "name": "claimableFeeCredit",
            "type": "u64"
          },
          {
            "name": "baseFeeRateNad",
            "type": "u64"
          },
          {
            "name": "divergenceFeeRateNad",
            "type": "u64"
          },
          {
            "name": "volatilityFeeRateNad",
            "type": "u64"
          },
          {
            "name": "totalFeeRateNad",
            "type": "u64"
          },
          {
            "name": "startPriceNad",
            "type": "u64"
          },
          {
            "name": "tradeEndPriceNad",
            "docs": [
              "Invariant-preserving executable trade endpoint."
            ],
            "type": "u64"
          },
          {
            "name": "reserveEndPriceNad",
            "docs": [
              "Final executable marginal price. A protected retained surcharge is",
              "non-quoteable, so it does not change this value."
            ],
            "type": "u64"
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
            "name": "decayedVolatilityNad",
            "type": "u64"
          },
          {
            "name": "postSuccessVolatilityNad",
            "type": "u64"
          },
          {
            "name": "retentionActive",
            "type": "bool"
          },
          {
            "name": "retentionTargetSaturated",
            "type": "bool"
          },
          {
            "name": "protectedRecenterBaseReserve",
            "type": "u64"
          },
          {
            "name": "protectedRecenterQuoteReserve",
            "type": "u64"
          },
          {
            "name": "protectedProfitPerShareNad",
            "type": "u128"
          },
          {
            "name": "projectedProtectedProfitPerShareNad",
            "type": "u128"
          },
          {
            "name": "retentionRequiredNad",
            "type": "u128"
          },
          {
            "name": "retentionStopNad",
            "type": "u128"
          },
          {
            "name": "retentionHardCapNad",
            "type": "u128"
          },
          {
            "name": "lowerRangePriceNad",
            "docs": [
              "Explicit range metadata. Zeroes denote the legacy curve during the",
              "temporary caller migration."
            ],
            "type": "u64"
          },
          {
            "name": "upperRangePriceNad",
            "type": "u64"
          },
          {
            "name": "explicitCurveBranch",
            "docs": [
              "0=lower tail, 1=concentrated band, 2=upper tail."
            ],
            "type": "u8"
          },
          {
            "name": "ordinaryBaseReserveNad",
            "type": "u128"
          },
          {
            "name": "ordinaryQuoteReserveNad",
            "type": "u128"
          },
          {
            "name": "finalSpotPriceNad",
            "type": "u64"
          },
          {
            "name": "baseHlpQuoteDebtDelta",
            "type": "i128"
          },
          {
            "name": "quoteHlpBaseDebtDelta",
            "type": "i128"
          },
          {
            "name": "hlpRecoveryTargetAsset",
            "docs": [
              "Yield-Basis-like recovery funded exclusively by the stressed hLP."
            ],
            "type": "u8"
          },
          {
            "name": "hlpRecoveryFundingGap",
            "type": "u64"
          },
          {
            "name": "hlpRecoveryMatchedInput",
            "type": "u64"
          },
          {
            "name": "hlpRecoveryBonusOutput",
            "type": "u64"
          },
          {
            "name": "hlpRecoveryDiscountBps",
            "type": "u16"
          },
          {
            "name": "hlpRecoveryCritical",
            "type": "bool"
          }
        ]
      }
    },
    {
      "name": "updateFutarchyAuthorityArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "newAuthority",
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "updateLeverageDelegationArgs",
      "type": {
        "kind": "struct",
        "fields": [
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
          }
        ]
      }
    },
    {
      "name": "updateProtocolAuctionConfigArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "lane",
            "type": {
              "defined": {
                "name": "protocolAuctionLane"
              }
            }
          },
          {
            "name": "acceptedMint",
            "type": {
              "option": "pubkey"
            }
          },
          {
            "name": "params",
            "type": {
              "option": {
                "defined": {
                  "name": "protocolAuctionParams"
                }
              }
            }
          }
        ]
      }
    },
    {
      "name": "updateProtocolAuctionRecipientsArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "lane",
            "type": {
              "defined": {
                "name": "protocolAuctionLane"
              }
            }
          },
          {
            "name": "treasury",
            "type": {
              "option": "pubkey"
            }
          },
          {
            "name": "stakingVault",
            "type": {
              "option": "pubkey"
            }
          },
          {
            "name": "treasuryBps",
            "type": {
              "option": "u16"
            }
          },
          {
            "name": "stakingVaultBps",
            "type": {
              "option": "u16"
            }
          }
        ]
      }
    },
    {
      "name": "updateProtocolAuctionRouteArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "lane",
            "type": {
              "defined": {
                "name": "protocolAuctionLane"
              }
            }
          },
          {
            "name": "soldMint",
            "type": "pubkey"
          },
          {
            "name": "referenceMarket",
            "docs": [
              "A governance-approved market key. The default key removes the approval",
              "and permits settlement only from the sold market's own direct pair."
            ],
            "type": "pubkey"
          }
        ]
      }
    },
    {
      "name": "updateProtocolRevenueArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "swapBps",
            "type": {
              "option": "u16"
            }
          },
          {
            "name": "interestBps",
            "type": {
              "option": "u16"
            }
          },
          {
            "name": "maxReferralInterestShareBps",
            "type": {
              "option": "u16"
            }
          },
          {
            "name": "revenueDistribution",
            "type": {
              "option": {
                "defined": {
                  "name": "revenueDistribution"
                }
              }
            }
          },
          {
            "name": "protocolAuctionSplit",
            "type": {
              "option": {
                "defined": {
                  "name": "protocolAuctionSplit"
                }
              }
            }
          }
        ]
      }
    },
    {
      "name": "updateRevenueRecipientsArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "futarchyTreasury",
            "type": {
              "option": "pubkey"
            }
          },
          {
            "name": "buybacksVault",
            "type": {
              "option": "pubkey"
            }
          },
          {
            "name": "teamTreasury",
            "type": {
              "option": "pubkey"
            }
          }
        ]
      }
    },
    {
      "name": "virtualYieldLedger",
      "docs": [
        "A yield ledger for yLP burned into one support position. Its checkpoints",
        "isolate each proposal lock while the user's ordinary YieldAccounts continue",
        "to follow only the live balance in their Token-2022 ATA."
      ],
      "type": {
        "kind": "struct",
        "fields": [
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
            "type": "u64"
          },
          {
            "name": "interestRemainderQ64",
            "type": "u64"
          }
        ]
      }
    },
    {
      "name": "withdrawCollateralArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "withdrawAmount",
            "type": "u64"
          },
          {
            "name": "minAssetAmountOut",
            "type": "u64"
          },
          {
            "name": "minLiquidationCfBps",
            "type": "u16"
          }
        ]
      }
    },
    {
      "name": "withdrawSingleSidedArgs",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "hlpAmount",
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
    },
    {
      "name": "yieldClaimed",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "lpMint",
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
            "name": "swapFeeAmount",
            "type": "u64"
          },
          {
            "name": "interestAmount",
            "type": "u64"
          },
          {
            "name": "recipientCredit",
            "docs": [
              "Total amount credited to the recipient after transfer fees."
            ],
            "type": "u64"
          },
          {
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "yieldRecipientUpdated",
      "type": {
        "kind": "struct",
        "fields": [
          {
            "name": "market",
            "type": "pubkey"
          },
          {
            "name": "owner",
            "type": "pubkey"
          },
          {
            "name": "lpMint",
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
            "name": "metadata",
            "type": {
              "defined": {
                "name": "marketEventMetadata"
              }
            }
          }
        ]
      }
    },
    {
      "name": "yieldTokenKind",
      "type": {
        "kind": "enum",
        "variants": [
          {
            "name": "ylp"
          },
          {
            "name": "hlp"
          }
        ]
      }
    }
  ],
  "constants": [
    {
      "name": "borrowPositionSeedPrefix",
      "type": "bytes",
      "value": "[98, 111, 114, 114, 111, 119, 95, 112, 111, 115, 105, 116, 105, 111, 110, 95, 118, 50]"
    },
    {
      "name": "bpsDenominator",
      "type": "u16",
      "value": "10000"
    },
    {
      "name": "futarchyAuthoritySeedPrefix",
      "type": "bytes",
      "value": "[102, 117, 116, 97, 114, 99, 104, 121, 95, 97, 117, 116, 104, 111, 114, 105, 116, 121]"
    },
    {
      "name": "hlpYlpVaultSeedPrefix",
      "type": "bytes",
      "value": "[104, 108, 112, 95, 121, 108, 112, 95, 118, 97, 117, 108, 116]"
    },
    {
      "name": "insuranceSeedPrefix",
      "type": "bytes",
      "value": "[105, 110, 115, 117, 114, 97, 110, 99, 101]"
    },
    {
      "name": "leverageCollateralVaultSeedPrefix",
      "type": "bytes",
      "value": "[108, 101, 118, 101, 114, 97, 103, 101, 95, 99, 111, 108, 108, 97, 116, 101, 114, 97, 108]"
    },
    {
      "name": "leverageDelegationSeedPrefix",
      "type": "bytes",
      "value": "[108, 101, 118, 101, 114, 97, 103, 101, 95, 100, 101, 108, 101, 103, 97, 116, 105, 111, 110, 95, 118, 50]"
    },
    {
      "name": "leverageInitialMarginBps",
      "type": "u16",
      "value": "1000"
    },
    {
      "name": "leverageMaintenanceBufferBps",
      "type": "u16",
      "value": "700"
    },
    {
      "name": "leverageMaxMultiplierBps",
      "type": "u64",
      "value": "200000"
    },
    {
      "name": "leverageMaxUnwindImpactBps",
      "type": "u16",
      "value": "200"
    },
    {
      "name": "leveragePositionSeedPrefix",
      "type": "bytes",
      "value": "[108, 101, 118, 101, 114, 97, 103, 101, 95, 112, 111, 115, 105, 116, 105, 111, 110, 95, 118, 50]"
    },
    {
      "name": "liquidationCloseFactorBps",
      "type": "u16",
      "value": "5000"
    },
    {
      "name": "liquidationIncentiveBps",
      "type": "u16",
      "value": "100"
    },
    {
      "name": "liquidationInsuranceFundingBps",
      "type": "u16",
      "value": "200"
    },
    {
      "name": "liquidationMaxIncentiveBps",
      "type": "u16",
      "value": "500"
    },
    {
      "name": "liquidationPenaltyBps",
      "type": "u16",
      "value": "300"
    },
    {
      "name": "ltvBufferBps",
      "type": "u16",
      "value": "500"
    },
    {
      "name": "marketCollateralVaultSeedPrefix",
      "type": "bytes",
      "value": "[109, 97, 114, 107, 101, 116, 95, 99, 111, 108, 108, 97, 116, 101, 114, 97, 108]"
    },
    {
      "name": "marketCreationFeeLamports",
      "type": "u64",
      "value": "200000000"
    },
    {
      "name": "marketInterestVaultSeedPrefix",
      "type": "bytes",
      "value": "[109, 97, 114, 107, 101, 116, 95, 105, 110, 116, 101, 114, 101, 115, 116]"
    },
    {
      "name": "marketLayoutVersion",
      "docs": [
        "Serialized `Market` account layout discriminator.",
        "",
        "Dusk is still pre-launch, so CONCENTRATED ships in the first deployable layout.",
        "Increment this only for an incompatible account-layout change after",
        "deployment, never for ordinary feature work or product naming."
      ],
      "type": "u8",
      "value": "1"
    },
    {
      "name": "marketReserveVaultSeedPrefix",
      "type": "bytes",
      "value": "[109, 97, 114, 107, 101, 116, 95, 114, 101, 115, 101, 114, 118, 101]"
    },
    {
      "name": "marketV2SeedPrefix",
      "type": "bytes",
      "value": "[109, 97, 114, 107, 101, 116, 95, 118, 50]"
    },
    {
      "name": "maxCollateralFactorBps",
      "type": "u16",
      "value": "8500"
    },
    {
      "name": "maxInsuranceDrawPerDayBps",
      "type": "u16",
      "value": "5000"
    },
    {
      "name": "maxInsuranceDrawPerEventBps",
      "docs": [
        "Absolute protocol ceilings for insurance-vault loss concentration. Market",
        "governance may lower either limit, including to zero, but may never raise",
        "them above these values."
      ],
      "type": "u16",
      "value": "2000"
    },
    {
      "name": "maxParameterFeeBps",
      "docs": [
        "Absolute cap shared by the three configurable swap-fee components.",
        "Their configured component caps must also sum to no more than this value."
      ],
      "type": "u16",
      "value": "5000"
    },
    {
      "name": "maxReferralInterestShareBps",
      "type": "u16",
      "value": "10000"
    },
    {
      "name": "metadataSeedPrefix",
      "type": "bytes",
      "value": "[109, 101, 116, 97, 100, 97, 116, 97]"
    },
    {
      "name": "nad",
      "docs": [
        "NAD: Nine-decimal fixed point unit (1e9 scaling), similar to WAD (1e18) by Maker."
      ],
      "type": "u64",
      "value": "1000000000"
    },
    {
      "name": "nadDecimals",
      "type": "u8",
      "value": "9"
    },
    {
      "name": "parameterExecutionMaxUtilizationBps",
      "type": "u64",
      "value": "8000"
    },
    {
      "name": "parameterProposalExecutionWindowSeconds",
      "type": "i64",
      "value": "604800"
    },
    {
      "name": "parameterProposalSeedPrefix",
      "type": "bytes",
      "value": "[112, 97, 114, 97, 109, 101, 116, 101, 114, 95, 112, 114, 111, 112, 111, 115, 97, 108]"
    },
    {
      "name": "parameterProposalSponsorBps",
      "docs": [
        "Direct-yLP parameter governance thresholds and wall-clock lifecycle."
      ],
      "type": "u16",
      "value": "100"
    },
    {
      "name": "parameterProposalSupportBps",
      "type": "u16",
      "value": "5000"
    },
    {
      "name": "parameterProposalTimelockSeconds",
      "type": "i64",
      "value": "604800"
    },
    {
      "name": "proposalSupportSeedPrefix",
      "type": "bytes",
      "value": "[112, 114, 111, 112, 111, 115, 97, 108, 95, 115, 117, 112, 112, 111, 114, 116]"
    },
    {
      "name": "referralAccrualSeedPrefix",
      "type": "bytes",
      "value": "[114, 101, 102, 101, 114, 114, 97, 108, 95, 97, 99, 99, 114, 117, 97, 108]"
    },
    {
      "name": "referralPartnerSeedPrefix",
      "type": "bytes",
      "value": "[114, 101, 102, 101, 114, 114, 97, 108, 95, 112, 97, 114, 116, 110, 101, 114]"
    },
    {
      "name": "targetMsPerSlot",
      "type": "u64",
      "value": "400"
    },
    {
      "name": "yieldAccountSeedPrefix",
      "type": "bytes",
      "value": "[121, 105, 101, 108, 100]"
    }
  ]
};
