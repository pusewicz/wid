module Wid
  class ParserFunctionDeclarationTest < ParserTest
    def test_function_declaration
      ast = parse(<<~WID)
        def square(x)
          x * x
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::FunctionDeclaration,
            name: {
              class: Nodes::Identifier,
              name: "square"
            },
            params: [
              {
                class: Nodes::Identifier,
                name: "x"
              }
            ],
            body: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::ExpressionStatement,
                  expression: {
                    class: Nodes::BinaryExpression,
                    operator: "*",
                    left: {
                      class: Nodes::Identifier,
                      name: "x"
                    },
                    right: {
                      class: Nodes::Identifier,
                      name: "x"
                    }
                  }
                }
              ]
            }
          }
        ]
      }, ast)
    end

    def test_function_declaration_with_explicit_return
      ast = parse(<<~WID)
        def square(x)
          return x * x
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::FunctionDeclaration,
            name: {
              class: Nodes::Identifier,
              name: "square"
            },
            params: [
              {
                class: Nodes::Identifier,
                name: "x"
              }
            ],
            body: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::ReturnStatement,
                  argument: {
                    class: Nodes::BinaryExpression,
                    operator: "*",
                    left: {
                      class: Nodes::Identifier,
                      name: "x"
                    },
                    right: {
                      class: Nodes::Identifier,
                      name: "x"
                    }
                  }
                }
              ]
            }
          }
        ]
      }, ast)
    end

    def test_function_declaration_with_no_return_value
      ast = parse(<<~WID)
        def foo()
          return
        end
      WID

      assert_ast_equal({
        class: Nodes::Program,
        body: [
          {
            class: Nodes::FunctionDeclaration,
            name: {
              class: Nodes::Identifier,
              name: "foo"
            },
            params: [],
            body: {
              class: Nodes::BlockStatement,
              body: [
                {
                  class: Nodes::ReturnStatement,
                  argument: nil
                }
              ]
            }
          }
        ]
      }, ast)
    end
  end
end
