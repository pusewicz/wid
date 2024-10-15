module Wid
  class ParserRelationalExpressionTest < ParserTest
    def test_more_than
      ast = parse("x > 0")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
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
            }
          }
        ]
      }, ast)
    end

    def test_less_than
      ast = parse("x < 0")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "<",
              left: {
                class: Nodes::Identifier,
                name: "x"
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 0
              }
            }
          }
        ]
      }, ast)
    end

    def test_more_than_or_equal
      ast = parse("x >= 0")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
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
            }
          }
        ]
      }, ast)
    end

    def test_less_than_or_equal
      ast = parse("x <= 0")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::BinaryExpression,
              operator: "<=",
              left: {
                class: Nodes::Identifier,
                name: "x"
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 0
              }
            }
          }
        ]
      }, ast)
    end
  end
end
