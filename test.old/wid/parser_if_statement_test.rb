module Wid
  class ParserIfStatementTest < ParserTest
    def test_if_statement
      ast = parse(<<~WID)
        if x
          x = 1
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::IfStatement,
            test: {
              class: Nodes::Identifier,
              name: "x"
            },
            consequent: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::ExpressionStatement,
                  expression: {
                    class: Nodes::AssignmentExpression,
                    operator: "=",
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
            },
            alternate: nil
          }
        ]
      }, ast)
    end

    def test_if_statement_with_optional_then
      ast = parse(<<~WID)
        if x then
          x = 1
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::IfStatement,
            test: {
              class: Nodes::Identifier,
              name: "x"
            },
            consequent: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::ExpressionStatement,
                  expression: {
                    class: Nodes::AssignmentExpression,
                    operator: "=",
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
            },
            alternate: nil
          }
        ]
      }, ast)
    end

    def test_if_statement_with_else
      ast = parse(<<~WID)
        if x
          x = 1
        else 
          x = 2
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::IfStatement,
            test: {
              class: Nodes::Identifier,
              name: "x"
            },
            consequent: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::ExpressionStatement,
                  expression: {
                    class: Nodes::AssignmentExpression,
                    operator: "=",
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
            },
            alternate: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::ExpressionStatement,
                  expression: {
                    class: Nodes::AssignmentExpression,
                    operator: "=",
                    left: {
                      class: Nodes::Identifier,
                      name: "x"
                    },
                    right: {
                      class: Nodes::NumericLiteral,
                      value: 2
                    }
                  }
                }
              ]
            }
          }
        ]
      }, ast)
    end

    def test_if_statement_relational_expression
      parse(<<~WID)
        if x > 10
          x = 0
        else
          x += 1
        end
      WID
    end
  end
end
