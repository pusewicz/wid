module Wid
  class ParserLogicalExpressionTest < ParserTest
    def test_logical_and_expression
      ast = parse("x > 0 && y < 1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::LogicalExpression,
              operator: "&&",
              left: {
                class: Nodes::BinaryExpression,
                operator: ">",
                left: {
                  class: Nodes::Identifier,
                  name: "x"
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 0
                }
              },
              right: {
                class: Nodes::BinaryExpression,
                operator: "<",
                left: {
                  class: Nodes::Identifier,
                  name: "y"
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 1
                }
              }
            }
          }
        ]
      }, ast)
    end

    def test_logical_or_expression
      ast = parse("x > 0 || y < 1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::LogicalExpression,
              operator: "||",
              left: {
                class: Nodes::BinaryExpression,
                operator: ">",
                left: {
                  class: Nodes::Identifier,
                  name: "x"
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 0
                }
              },
              right: {
                class: Nodes::BinaryExpression,
                operator: "<",
                left: {
                  class: Nodes::Identifier,
                  name: "y"
                },
                right: {
                  class: Nodes::NumericLiteral,
                  value: 1
                }
              }
            }
          }
        ]
      }, ast)
    end
  end
end
