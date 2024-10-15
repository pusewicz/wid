module Wid
  class ParserUnaryxpressionTest < ParserTest
    def text_unary_minus
      ast = parse("-1")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::UnaryExpression,
              operator: "-",
              argument: {
                class: Nodes::NumericLiteral,
                value: 1
              }
            }
          }
        ]
      }, ast)
    end

    def test_unary_not
      ast = parse("!true")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::UnaryExpression,
              operator: "!",
              argument: {
                class: Nodes::BooleanLiteral,
                value: true
              }
            }
          }
        ]
      }, ast)
    end

    def test_unary_chained
      ast = parse("!!true")

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::ExpressionStatement,
            expression: {
              class: Nodes::UnaryExpression,
              operator: "!",
              argument: {
                class: Nodes::UnaryExpression,
                operator: "!",
                argument: {
                  class: Nodes::BooleanLiteral,
                  value: true
                }
              }
            }
          }
        ]
      }, ast)
    end
  end
end
