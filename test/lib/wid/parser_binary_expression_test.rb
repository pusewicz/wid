module Wid
  class ParserBinaryExpressionTest < ParserTest
    def test_binary_expression_additive
      ast = parse("1 + 2")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "+",
              left: {
                class: Nodes::NumericLiteral,
                value: 1
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 2
              }
            }
          }
        ]
      }, ast)
    end

    # left: 1 + 2
    # right: 3
    def test_binary_expression_chained
      ast = parse("1 + 2 - 3")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "-",
              left: {
                class: Nodes::BinaryExpression,
                operator: "+",
                left: {
                  class: Nodes::NumericLiteral,
                  value: 1
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 2
                }
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 3
              }
            }
          }
        ]
      }, ast)
    end

    def test_binary_expression_multiplicative
      ast = parse("1 * 2")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "*",
              left: {
                class: Nodes::NumericLiteral,
                value: 1
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 2
              }
            }
          }
        ]
      }, ast)
    end

    def test_binary_expression_multiplicative_chained
      ast = parse("1 + 2 * 3")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "+",
              left: {
                class: Nodes::NumericLiteral,
                value: 1
              },
              right: {
                class: Nodes::BinaryExpression,
                operator: "*",
                left: {
                  class: Nodes::NumericLiteral,
                  value: 2
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 3
                }
              }
            }
          }
        ]
      }, ast)
    end

    def test_binary_expression_multiplicative_chained_equal
      ast = parse("1 * 2 * 3")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "*",
              left: {
                class: Nodes::BinaryExpression,
                operator: "*",
                left: {
                  class: Nodes::NumericLiteral,
                  value: 1
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 2
                }
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 3
              }
            }
          }
        ]
      }, ast)
    end

    def test_binary_expression_grouped
      ast = parse("(1 + 2) * 3")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "*",
              left: {
                class: Nodes::BinaryExpression,
                operator: "+",
                left: {
                  class: Nodes::NumericLiteral,
                  value: 1
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 2
                }
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 3
              }
            }
          }
        ]
      }, ast)
    end
  end
end
