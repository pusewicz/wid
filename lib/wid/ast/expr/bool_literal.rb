module Wid
  module AST
    class Expr
      class BoolLiteral < Literal
        def initialize(value:)
          value = value == "true"
          super
        end
      end
    end
  end
end
