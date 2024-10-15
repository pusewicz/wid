module Wid
  class ParserEqualityExpressionTest < ParserTest
    def test_equality_equal_equal
      ast = parse("x > 0 == true")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "==",
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
                class: Nodes::BooleanLiteral,
                value: true
              }
            }
          }
        ]
      }, ast)
    end

    def test_equality_not_equal
      ast = parse("x >= 0 != false")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "!=",
              left: {
                class: Nodes::BinaryExpression,
                operator: ">=",
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
                class: Nodes::BooleanLiteral,
                value: false
              }
            }
          }
        ]
      }, ast)
    end
  end
end
