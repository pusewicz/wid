module Wid
  class ParserWhileTest < ParserTest
    def test_while
      ast = parse(<<~WID)
        while x > 10
          x -= 1
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::WhileStatement,
            test: {
              class: Nodes::BinaryExpression,
              operator: ">",
              left: {
                class: Nodes::Identifier,
                name: "x"
              },
              right: {
                class: Nodes::NumericLiteral,
                value: 10
              }
            },
            body: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::ExpressionStatement,
                  expression: {
                    class: Nodes::AssignmentExpression,
                    operator: "-=",
                    left: {
                      class: Nodes::Identifier,
                      name: "x"
                    },
                    right: {
                      class: Nodes::NumericLiteral,
                      value: 1
                    }
                  }
                }
              ]
            }
          }
        ]
      }, ast)
    end
  end
end
